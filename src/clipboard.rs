//! System clipboard access.
//!
//! Wrapped rather than used directly so a missing or broken clipboard backend
//! degrades to a no-op instead of taking the terminal down with it — on Linux
//! there may be no display server at all.

pub struct Clipboard {
    inner: Option<arboard::Clipboard>,
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

    pub fn set(&mut self, text: &str) {
        let Some(clipboard) = self.inner.as_mut() else {
            return;
        };
        if let Err(err) = clipboard.set_text(text.to_string()) {
            log::warn!("clipboard write failed: {err}");
        }
    }

    /// Writes to the X11/Wayland primary selection, which is what
    /// middle-click pastes from. A no-op elsewhere: macOS has no equivalent
    /// and copy-on-select is not a convention there.
    #[cfg(target_os = "linux")]
    pub fn set_primary(&mut self, text: &str) {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        let Some(clipboard) = self.inner.as_mut() else {
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

    #[cfg(not(target_os = "linux"))]
    pub fn set_primary(&mut self, _text: &str) {}

    #[cfg(target_os = "linux")]
    pub fn get_primary(&mut self) -> Option<String> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        let clipboard = self.inner.as_mut()?;
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
        let clipboard = self.inner.as_mut()?;
        match clipboard.get_text() {
            Ok(text) => Some(text),
            Err(err) => {
                log::warn!("clipboard read failed: {err}");
                None
            },
        }
    }
}
