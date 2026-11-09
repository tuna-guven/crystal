//! Crystal's own clipboard (docs/decisions/0041-internal-clipboard-only.md):
//! `y` in the pager copies the note into zeroizing memory that never
//! leaves Crystal; the editor pastes from it (Phase 4). It is wiped on
//! quit, and on lock once that exists (Phase 5).
//!
//! Crystal never writes to the system clipboard: copying to other
//! programs is the terminal's own selection (select with the mouse, then
//! the terminal's copy key), on what Crystal shows.

use zeroize::Zeroizing;

#[derive(Default)]
pub struct Clipboard {
    text: Option<Zeroizing<String>>,
}

impl Clipboard {
    /// Replaces the clipboard with `text`, wiping what it held.
    pub fn set(&mut self, text: &str) {
        self.text = Some(Zeroizing::new(text.to_owned()));
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the editor pastes from it (Phase 4)")
    )]
    pub fn get(&self) -> Option<&str> {
        self.text.as_deref().map(String::as_str)
    }

    /// Wipes the clipboard (quit, lock).
    pub fn wipe(&mut self) {
        self.text = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_wipes_and_replaces() {
        let mut c = Clipboard::default();
        assert_eq!(c.get(), None);
        c.set("first");
        c.set("second");
        assert_eq!(c.get(), Some("second"));
        c.wipe();
        assert_eq!(c.get(), None);
    }
}
