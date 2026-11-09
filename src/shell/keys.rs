//! Terminal input bytes to keys, for the shell (docs/decisions/0033-shell.md).
//!
//! Raw mode delivers bytes: printable UTF-8, control bytes, and escape
//! sequences for special keys (`ESC [ A` for Up, `ESC [ 3 ~` for Delete,
//! ...). A sequence or a UTF-8 character can be split across two reads, so
//! the decoder keeps the unfinished bytes. Bracketed paste (`ESC [ 200 ~`
//! ... `ESC [ 201 ~`, enabled by the screen) arrives as one `Paste`: pasted
//! text is never taken as keys, so a pasted escape sequence or newline
//! cannot run a command. Unknown sequences are dropped.
//!
//! Every buffer here holds typed or pasted text: zeroizing, fixed size.

use zeroize::Zeroizing;

/// The longest paste kept, in bytes; the rest is dropped.
pub const MAX_PASTE: usize = 4096;
/// The longest unfinished escape sequence kept.
const MAX_PENDING: usize = 16;

const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    /// Shift+Tab.
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Esc,
    /// Ctrl and a letter, as the letter (`Ctrl('c')`).
    Ctrl(char),
    /// Pasted text, in zeroizing memory.
    Paste(Zeroizing<String>),
}

/// Typed text: shown only as a kind, never its content.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(_) => f.write_str("Char(..)"),
            Self::Paste(_) => f.write_str("Paste(..)"),
            Self::Ctrl(c) => write!(f, "Ctrl({c})"),
            other => f.write_str(match other {
                Self::Enter => "Enter",
                Self::Tab => "Tab",
                Self::BackTab => "BackTab",
                Self::Backspace => "Backspace",
                Self::Delete => "Delete",
                Self::Left => "Left",
                Self::Right => "Right",
                Self::Up => "Up",
                Self::Down => "Down",
                Self::Home => "Home",
                Self::End => "End",
                Self::PageUp => "PageUp",
                Self::PageDown => "PageDown",
                _ => "Esc",
            }),
        }
    }
}

/// Keeps the bytes of an unfinished sequence, character or paste.
pub struct Decoder {
    pending: Zeroizing<Vec<u8>>,
    /// Inside a bracketed paste: its bytes so far.
    paste: Option<Zeroizing<Vec<u8>>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            pending: Zeroizing::new(Vec::with_capacity(MAX_PENDING)),
            paste: None,
        }
    }

    /// Decodes `bytes` (one read) into keys, appended to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Key>) {
        for &byte in bytes {
            if let Some(paste) = &mut self.paste {
                if paste.len() < MAX_PASTE + PASTE_END.len() {
                    paste.push(byte);
                } else {
                    // Over the limit: keep only a window for the end marker.
                    paste.copy_within(1.., 0);
                    if let Some(last) = paste.last_mut() {
                        *last = byte;
                    }
                }
                if paste.ends_with(PASTE_END) {
                    let mut paste = self.paste.take().unwrap_or_default();
                    let len = paste.len() - PASTE_END.len();
                    out.push(Key::Paste(pasted_text(&paste[..len.min(MAX_PASTE)])));
                    zeroize::Zeroize::zeroize(&mut *paste);
                }
                continue;
            }
            if self.pending.len() == MAX_PENDING {
                // Nothing this long is a key: drop it.
                zeroize::Zeroize::zeroize(&mut *self.pending);
                self.pending.clear();
            }
            self.pending.push(byte);
            match decode(&self.pending) {
                Decoded::More => {}
                Decoded::Key(key) => {
                    out.push(key);
                    self.clear_pending();
                }
                Decoded::PasteStart => {
                    self.paste = Some(Zeroizing::new(Vec::with_capacity(
                        MAX_PASTE + PASTE_END.len(),
                    )));
                    self.clear_pending();
                }
                Decoded::Drop => self.clear_pending(),
            }
        }
        // A lone ESC at the end of a read is the Esc key: terminals send a
        // whole sequence in one write.
        if self.pending.as_slice() == b"\x1b" {
            out.push(Key::Esc);
            self.clear_pending();
        }
    }

    fn clear_pending(&mut self) {
        zeroize::Zeroize::zeroize(&mut *self.pending);
        self.pending.clear();
    }
}

/// The paste as text: invalid UTF-8 replaced, line breaks and tabs turned
/// into spaces (the prompt is one line), other control characters dropped.
fn pasted_text(bytes: &[u8]) -> Zeroizing<String> {
    let mut text = Zeroizing::new(String::with_capacity(bytes.len()));
    for c in String::from_utf8_lossy(bytes).chars() {
        match c {
            '\n' | '\r' | '\t' => text.push(' '),
            c if crate::ui::grid::is_forbidden(c) => {}
            c => text.push(c),
        }
    }
    text
}

enum Decoded {
    More,
    Key(Key),
    PasteStart,
    Drop,
}

fn decode(bytes: &[u8]) -> Decoded {
    match bytes {
        [] => Decoded::More,
        [b'\r' | b'\n'] => Decoded::Key(Key::Enter),
        [b'\t'] => Decoded::Key(Key::Tab),
        [0x7f | 0x08] => Decoded::Key(Key::Backspace),
        [b @ 0x01..=0x1a] => Decoded::Key(Key::Ctrl(char::from(b'a' + b - 1))),
        [0x1b] => Decoded::More,
        [0x1b, b'[' | b'O'] => Decoded::More,
        [0x1b, b'O', last] => simple(*last),
        [0x1b, b'[', rest @ ..] => csi(rest),
        // Alt and a key: not used; dropped.
        [0x1b, _] => Decoded::Drop,
        [first, ..] if *first < 0x20 => Decoded::Drop,
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s
                .chars()
                .next()
                .map_or(Decoded::Drop, |c| Decoded::Key(Key::Char(c))),
            // An unfinished character (at most 4 bytes) waits for more.
            Err(e) if e.error_len().is_none() && bytes.len() < 4 => Decoded::More,
            Err(_) => Decoded::Drop,
        },
    }
}

/// `ESC [ X` or `ESC O X`.
fn simple(last: u8) -> Decoded {
    Decoded::Key(match last {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        _ => return Decoded::Drop,
    })
}

/// After `ESC [`: parameters (digits and `;`), then one final byte.
fn csi(rest: &[u8]) -> Decoded {
    let Some((&last, params)) = rest.split_last() else {
        return Decoded::More;
    };
    if last.is_ascii_digit() || last == b';' {
        return Decoded::More;
    }
    if !params.iter().all(|b| b.is_ascii_digit() || *b == b';') {
        return Decoded::Drop;
    }
    match (params, last) {
        (b"200", b'~') => Decoded::PasteStart,
        (b"", b'Z') => Decoded::Key(Key::BackTab),
        (b"3", b'~') => Decoded::Key(Key::Delete),
        (b"1" | b"7", b'~') => Decoded::Key(Key::Home),
        (b"4" | b"8", b'~') => Decoded::Key(Key::End),
        (b"5", b'~') => Decoded::Key(Key::PageUp),
        (b"6", b'~') => Decoded::Key(Key::PageDown),
        // Modified arrows (`ESC [ 1 ; 5 C`) act as plain ones.
        (_, b'A' | b'B' | b'C' | b'D' | b'H' | b'F') => simple(last),
        _ => Decoded::Drop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(chunks: &[&[u8]]) -> Vec<Key> {
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        for chunk in chunks {
            decoder.feed(chunk, &mut out);
        }
        out
    }

    fn text(key: &Key) -> Option<String> {
        match key {
            Key::Char(c) => Some(c.to_string()),
            Key::Paste(p) => Some(p.as_str().to_owned()),
            _ => None,
        }
    }

    #[test]
    fn plain_and_control_keys() {
        let got = keys(&[b"ab\r\t\x7f\x03\x04"]);
        assert_eq!(
            got,
            [
                Key::Char('a'),
                Key::Char('b'),
                Key::Enter,
                Key::Tab,
                Key::Backspace,
                Key::Ctrl('c'),
                Key::Ctrl('d')
            ]
        );
    }

    #[test]
    fn special_keys() {
        let cases: [(&[u8], Key); 15] = [
            (b"\x1b[A", Key::Up),
            (b"\x1b[B", Key::Down),
            (b"\x1b[C", Key::Right),
            (b"\x1b[D", Key::Left),
            (b"\x1bOH", Key::Home),
            (b"\x1b[F", Key::End),
            (b"\x1b[1~", Key::Home),
            (b"\x1b[4~", Key::End),
            (b"\x1b[3~", Key::Delete),
            (b"\x1b[5~", Key::PageUp),
            (b"\x1b[6~", Key::PageDown),
            (b"\x1b[1;5C", Key::Right),
            (b"\x1b", Key::Esc),
            (b"\x1b[7~", Key::Home),
            (b"\x1b[Z", Key::BackTab),
        ];
        for (bytes, key) in cases {
            assert_eq!(keys(&[bytes]), [key], "{bytes:?}");
        }
    }

    #[test]
    fn split_sequences_and_characters() {
        assert_eq!(keys(&[b"\x1b[", b"3~"]), [Key::Delete]);
        assert_eq!(keys(&[b"\xe6\x97", b"\xa5"]), [Key::Char('日')]);
        assert_eq!(keys(&[b"\x1b[1;", b"5D"]), [Key::Left]);
    }

    #[test]
    fn unknown_input_is_dropped() {
        assert_eq!(keys(&[b"\x1b[99z", b"x"]), [Key::Char('x')]);
        assert_eq!(keys(&[b"\x1bx", b"y"]), [Key::Char('y')]);
        assert_eq!(keys(&[b"\xff", b"z"]), [Key::Char('z')]);
        // An endless sequence is cut off, never kept growing.
        let long = [b"\x1b[".as_slice(), &[b'1'; 100], b"A"].concat();
        let got = keys(&[&long, b"q"]);
        assert_eq!(got.last(), Some(&Key::Char('q')));
    }

    // A paste arrives whole, as text: its escape sequences and newlines
    // are not keys, even split across reads.
    #[test]
    fn bracketed_paste() {
        let got = keys(&[b"x\x1b[200~line one\nline \x1b[2Jtwo\x1b[2", b"01~y"]);
        assert_eq!(got.len(), 3);
        assert_eq!(text(&got[0]).as_deref(), Some("x"));
        assert_eq!(text(&got[1]).as_deref(), Some("line one line [2Jtwo"));
        assert_eq!(text(&got[2]).as_deref(), Some("y"));
    }

    #[test]
    fn huge_paste_is_cut_and_still_ends() {
        let big = vec![b'a'; MAX_PASTE * 3];
        let got = keys(&[b"\x1b[200~", &big, b"\x1b[201~", b"z"]);
        assert_eq!(got.len(), 2);
        assert!(text(&got[0]).unwrap().len() <= MAX_PASTE);
        assert_eq!(got[1], Key::Char('z'));
    }

    proptest::proptest! {
        // Any bytes, split into reads anywhere: no panic, and a paste never
        // carries a control or BiDi character into the line.
        #[test]
        fn any_input_decodes_safely(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400),
            cuts in proptest::collection::vec(0usize..400, 0..8),
        ) {
            let mut cuts = cuts;
            cuts.push(bytes.len());
            cuts.sort_unstable();
            let mut decoder = Decoder::new();
            let mut out = Vec::new();
            let mut at = 0;
            for cut in cuts {
                let cut = cut.min(bytes.len());
                if cut > at {
                    decoder.feed(&bytes[at..cut], &mut out);
                    at = cut;
                }
            }
            for key in &out {
                if let Key::Paste(text) = key {
                    proptest::prop_assert!(!text.contains(crate::ui::grid::is_forbidden));
                    proptest::prop_assert!(text.len() <= MAX_PASTE * 3);
                }
            }
        }
    }

    #[test]
    fn debug_never_shows_text() {
        let shown = format!("{:?}", keys(&[b"h", b"\x1b[200~hunter2\x1b[201~"]));
        assert!(
            !shown.contains('h') || !shown.contains("hunter2"),
            "{shown}"
        );
        assert!(!shown.contains("hunter2"));
    }
}
