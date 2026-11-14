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

use zeroize::{Zeroize, Zeroizing};

/// The longest paste kept, in bytes: a note's body limit
/// (docs/decisions/0045-simple-editor.md). The rest is dropped and the
/// paste marked as cut.
pub const MAX_PASTE: usize = crate::vault::codec::MAX_BODY_BYTES;
/// The paste buffer's fixed size: the paste and its end marker.
const PASTE_BUFFER: usize = MAX_PASTE + PASTE_END.len();
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
    /// Shift and an arrow, Home or End: selecting in the editor
    /// (docs/decisions/0045-simple-editor.md).
    ShiftLeft,
    ShiftRight,
    ShiftUp,
    ShiftDown,
    ShiftHome,
    ShiftEnd,
    Esc,
    /// Ctrl and a letter, as the letter (`Ctrl('c')`).
    Ctrl(char),
    /// Pasted text, in zeroizing memory; `cut` when it was longer than
    /// `MAX_PASTE` and the rest was dropped.
    Paste {
        text: Zeroizing<String>,
        cut: bool,
    },
}

/// Typed text: shown only as a kind, never its content.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(_) => f.write_str("Char(..)"),
            Self::Paste { .. } => f.write_str("Paste(..)"),
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
                Self::ShiftLeft => "ShiftLeft",
                Self::ShiftRight => "ShiftRight",
                Self::ShiftUp => "ShiftUp",
                Self::ShiftDown => "ShiftDown",
                Self::ShiftHome => "ShiftHome",
                Self::ShiftEnd => "ShiftEnd",
                _ => "Esc",
            }),
        }
    }
}

impl Key {
    /// The key without Shift, for the shell and the pager, which do not
    /// select: Shift+Up scrolls like Up.
    pub fn without_shift(self) -> Self {
        match self {
            Self::ShiftLeft => Self::Left,
            Self::ShiftRight => Self::Right,
            Self::ShiftUp => Self::Up,
            Self::ShiftDown => Self::Down,
            Self::ShiftHome => Self::Home,
            Self::ShiftEnd => Self::End,
            other => other,
        }
    }
}

/// Keeps the bytes of an unfinished sequence, character or paste.
pub struct Decoder {
    pending: Zeroizing<Vec<u8>>,
    /// Inside a bracketed paste: its bytes so far.
    paste: Option<Zeroizing<Vec<u8>>>,
    /// Once the paste buffer is full: the last bytes seen, to find the
    /// end marker without keeping the rest.
    window: Option<[u8; PASTE_END.len()]>,
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
            window: None,
        }
    }

    /// Decodes `bytes` (one read) into keys, appended to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Key>) {
        for &byte in bytes {
            if let Some(paste) = &mut self.paste {
                let ended = if let Some(window) = &mut self.window {
                    shift_in(window, byte)
                } else if paste.len() < PASTE_BUFFER {
                    // Within the buffer's capacity: never reallocates.
                    paste.push(byte);
                    paste.ends_with(PASTE_END)
                } else {
                    // Full: from here on only a window of the last bytes
                    // is kept, which costs nothing per byte however long
                    // the paste.
                    let mut window = [0; PASTE_END.len()];
                    window.copy_from_slice(&paste[PASTE_BUFFER - PASTE_END.len()..]);
                    let ended = shift_in(&mut window, byte);
                    self.window = Some(window);
                    ended
                };
                if ended {
                    let cut = self.window.take().map(|mut w| w.zeroize()).is_some();
                    let mut paste = self.paste.take().unwrap_or_default();
                    let len = if cut {
                        MAX_PASTE
                    } else {
                        paste.len() - PASTE_END.len()
                    };
                    out.push(Key::Paste {
                        text: pasted_text(&paste[..len]),
                        cut,
                    });
                    paste.zeroize();
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
                    self.paste = Some(Zeroizing::new(Vec::with_capacity(PASTE_BUFFER)));
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

/// Shifts `byte` into the end of `window`; true when it now holds the
/// paste's end marker.
fn shift_in(window: &mut [u8; PASTE_END.len()], byte: u8) -> bool {
    window.copy_within(1.., 0);
    window[PASTE_END.len() - 1] = byte;
    window == PASTE_END
}

/// The paste as text: invalid UTF-8 replaced by U+FFFD, line breaks kept
/// as `\n` (`\r\n` and a lone `\r` too), tabs kept, other control
/// characters dropped. The editor inserts it as it is; the shell's prompt
/// flattens it with `one_line`. A first pass measures the text, so the
/// zeroizing buffer is allocated once at its exact size; `utf8_chunks`
/// reads the bytes in place, so no unwiped copy is made.
fn pasted_text(bytes: &[u8]) -> Zeroizing<String> {
    let mut len = 0;
    clean_paste(bytes, |c| len += c.len_utf8());
    let mut text = Zeroizing::new(String::with_capacity(len));
    clean_paste(bytes, |c| text.push(c));
    text
}

/// Calls `emit` with each character `pasted_text` keeps.
fn clean_paste(bytes: &[u8], mut emit: impl FnMut(char)) {
    let mut after_cr = false;
    for chunk in bytes.utf8_chunks() {
        let invalid = (!chunk.invalid().is_empty()).then_some('\u{fffd}');
        for c in chunk.valid().chars().chain(invalid) {
            let cr = std::mem::replace(&mut after_cr, c == '\r');
            match c {
                '\r' => emit('\n'),
                '\n' if cr => {}
                '\n' | '\t' => emit(c),
                c if crate::ui::grid::is_forbidden(c) => {}
                c => emit(c),
            }
        }
    }
}

/// A paste for the shell's one-line prompt: line breaks and tabs become
/// spaces.
pub fn one_line(text: &str) -> Zeroizing<String> {
    let mut line = Zeroizing::new(String::with_capacity(text.len()));
    for c in text.chars() {
        line.push(if matches!(c, '\n' | '\t') { ' ' } else { c });
    }
    line
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
        // Shift (modifier 2, `ESC [ 1 ; 2 C`) selects; other modified
        // arrows (`ESC [ 1 ; 5 C`) act as plain ones.
        (b"1;2", _) => match simple(last) {
            Decoded::Key(key) => Decoded::Key(shifted(key)),
            other => other,
        },
        (_, b'A' | b'B' | b'C' | b'D' | b'H' | b'F') => simple(last),
        _ => Decoded::Drop,
    }
}

/// The Shift variant of an arrow, Home or End.
fn shifted(key: Key) -> Key {
    match key {
        Key::Left => Key::ShiftLeft,
        Key::Right => Key::ShiftRight,
        Key::Up => Key::ShiftUp,
        Key::Down => Key::ShiftDown,
        Key::Home => Key::ShiftHome,
        Key::End => Key::ShiftEnd,
        other => other,
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
            Key::Paste { text, .. } => Some(text.as_str().to_owned()),
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
        let cases: [(&[u8], Key); 22] = [
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
            (b"\x1b[1;2D", Key::ShiftLeft),
            (b"\x1b[1;2C", Key::ShiftRight),
            (b"\x1b[1;2A", Key::ShiftUp),
            (b"\x1b[1;2B", Key::ShiftDown),
            (b"\x1b[1;2H", Key::ShiftHome),
            (b"\x1b[1;2F", Key::ShiftEnd),
            (b"\x1b[1;6C", Key::Right),
        ];
        for key in [Key::ShiftLeft, Key::ShiftHome, Key::Char('x')] {
            let plain = matches!(key, Key::Char(_));
            assert_eq!(key.without_shift() == Key::Char('x'), plain);
        }
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
        assert_eq!(text(&got[1]).as_deref(), Some("line one\nline [2Jtwo"));
        assert_eq!(text(&got[2]).as_deref(), Some("y"));
    }

    // Line breaks arrive as `\n` whatever the terminal sent; tabs stay;
    // invalid UTF-8 becomes U+FFFD. The shell flattens it all to one line.
    #[test]
    fn paste_keeps_line_breaks_and_tabs() {
        let got = keys(&[b"\x1b[200~a\r\nb\rc\nd\te\xffz\x1b[201~"]);
        let pasted = text(&got[0]).unwrap();
        assert_eq!(pasted, "a\nb\nc\nd\te\u{fffd}z");
        assert_eq!(one_line(&pasted).as_str(), "a b c d e\u{fffd}z");
    }

    #[test]
    fn long_pastes_are_cut_at_the_limit_and_say_so() {
        let exact = vec![b'a'; MAX_PASTE];
        let got = keys(&[b"\x1b[200~", &exact, b"\x1b[201~"]);
        assert!(
            matches!(&got[..], [Key::Paste { text, cut: false }] if text.len() == MAX_PASTE),
            "{got:?}"
        );
        // Longer by less than the end marker (which then straddles the
        // buffer's end), and by more, with the marker split across reads:
        // cut to the limit, and the keys after it still arrive.
        for extra in [3, 100] {
            let long = vec![b'b'; MAX_PASTE + extra];
            let got = keys(&[b"\x1b[200~", &long, b"\x1b[20", b"1~z"]);
            assert!(
                matches!(
                    &got[..],
                    [Key::Paste { text, cut: true }, Key::Char('z')] if text.len() == MAX_PASTE
                ),
                "{extra}: {got:?}"
            );
        }
    }

    proptest::proptest! {
        // Any bytes, split into reads anywhere: no panic, and a paste never
        // carries a control or BiDi character other than `\n` and `\t`.
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
                if let Key::Paste { text, .. } = key {
                    let hidden = text.contains(|c: char| {
                        c != '\n' && c != '\t' && crate::ui::grid::is_forbidden(c)
                    });
                    proptest::prop_assert!(!hidden);
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
