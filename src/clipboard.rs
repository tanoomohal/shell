//! System clipboard access.
//!
//! Wrapped rather than used directly so a missing or broken clipboard backend
//! degrades gracefully — with native pbcopy/pbpaste on macOS as a rock-solid
//! fallback.

pub struct Clipboard {
    inner: Option<arboard::Clipboard>,
}

#[cfg(target_os = "macos")]
fn macos_pbcopy(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if let Ok(mut child) = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    } else {
        false
    }
}

#[cfg(target_os = "macos")]
fn macos_pbpaste() -> Option<String> {
    use std::process::Command;
    let output = Command::new("pbpaste").output().ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

impl Clipboard {
    pub fn new() -> Self {
        let inner = match arboard::Clipboard::new() {
            Ok(clipboard) => Some(clipboard),
            Err(err) => {
                log::warn!("clipboard unavailable: {err}");
                None
            },
        };
        Self { inner }
    }

    fn get_or_init(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.inner.is_none() {
            if let Ok(cb) = arboard::Clipboard::new() {
                self.inner = Some(cb);
            }
        }
        self.inner.as_mut()
    }

    pub fn set(&mut self, text: &str) {
        let mut ok = false;
        if let Some(clipboard) = self.get_or_init() {
            if clipboard.set_text(text.to_string()).is_ok() {
                ok = true;
            }
        }
        #[cfg(target_os = "macos")]
        if !ok {
            macos_pbcopy(text);
        }
    }

    /// Writes to the X11/Wayland primary selection, which is what
    /// middle-click pastes from. On macOS, copies to the system clipboard
    /// so drag-select is immediately copy-ready.
    #[cfg(target_os = "linux")]
    pub fn set_primary(&mut self, text: &str) {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        let Some(clipboard) = self.get_or_init() else {
            return;
        };
        if let Err(err) = clipboard
            .set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text.to_string())
        {
            log::warn!("primary selection write failed: {err}");
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_primary(&mut self, text: &str) {
        self.set(text);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn set_primary(&mut self, _text: &str) {}

    #[cfg(target_os = "linux")]
    pub fn get_primary(&mut self) -> Option<String> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        let clipboard = self.get_or_init()?;
        clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .ok()
    }

    /// Falls back to the ordinary clipboard where there is no primary
    /// selection, so middle-click still does something sensible.
    #[cfg(not(target_os = "linux"))]
    pub fn get_primary(&mut self) -> Option<String> {
        self.get()
    }

    pub fn get(&mut self) -> Option<String> {
        if let Some(clipboard) = self.get_or_init() {
            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            macos_pbpaste()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_operations() {
        let mut cb = Clipboard::new();
        cb.set("test_clipboard_roundtrip_string");
        let read = cb.get();
        assert_eq!(read.as_deref(), Some("test_clipboard_roundtrip_string"));

        #[cfg(target_os = "macos")]
        {
            assert!(macos_pbcopy("test_pbcopy_pbpaste_roundtrip"));
            assert_eq!(macos_pbpaste().as_deref(), Some("test_pbcopy_pbpaste_roundtrip"));
        }
    }
}
