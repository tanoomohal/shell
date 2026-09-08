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
