//! Terminal input bytes to keys, for the shell (decision 0033).
//!
//! Raw mode delivers bytes: printable UTF-8, control bytes, and escape
//! sequences for special keys (`ESC [ A` for Up, `ESC [ 3 ~` for Delete,
//! ...). A sequence or a UTF-8 character can be split across two reads, so
//! the decoder keeps the unfinished bytes. Bracketed paste (`ESC [ 200 ~`
//! ... `ESC [ 201 ~`, enabled by the screen) arrives as one `Paste`: pasted
//! text is never taken as keys, so a pasted escape sequence or newline
//! cannot run a command. Unknown sequences are dropped. Mouse reports
//! (`ESC [ < b ; x ; y M`), which only the editor turns on, arrive as
//! `Mouse` (decision 0073); inside a paste they are text.
//!
//! Every buffer here holds typed or pasted text: zeroizing, fixed size.

use zeroize::{Zeroize, Zeroizing};

/// The longest paste kept, in bytes: a note's body limit
/// (decision 0045). The rest is dropped and the
/// paste marked as cut.
pub const MAX_PASTE: usize = crate::vault::codec::MAX_BODY_BYTES;
/// The paste buffer's fixed size: the paste and its end marker.
const PASTE_BUFFER: usize = MAX_PASTE + PASTE_END.len();
/// The longest unfinished escape sequence kept: a mouse report with a
/// 3-digit button and 5-digit cells is 19 bytes (decision 0073).
const MAX_PENDING: usize = 24;

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
    /// (decision 0045).
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
    /// `MAX_PASTE` and the rest was dropped; `cleaned` when the cleaning
    /// dropped a control or BiDi character or replaced broken bytes, so
    /// the text differs from what was copied
    /// (decision 0047). Line breaks turned into `\n`
    /// do not count: they are expected.
    Paste {
        text: Zeroizing<String>,
        cut: bool,
        cleaned: bool,
    },
    /// A mouse report (decision 0073): what happened, at which cell,
    /// counted from 0. Only the editor turns reports on and acts on them.
    Mouse {
        event: MouseEvent,
        column: u16,
        row: u16,
    },
}

/// What a mouse report says. Only the left button and the wheel are
/// kept, without Shift, Alt or Ctrl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Press,
    /// Moved with the left button held.
    Drag,
    Release,
    WheelUp,
    WheelDown,
}

/// Typed text: shown only as a kind, never its content.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(_) => f.write_str("Char(..)"),
            Self::Paste { .. } => f.write_str("Paste(..)"),
            Self::Ctrl(c) => write!(f, "Ctrl({c})"),
            Self::Mouse { event, .. } => write!(f, "Mouse({event:?})"),
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
    /// Inside a CSI sequence longer than `MAX_PENDING`: its bytes are
    /// skipped up to its final byte, so no part of it becomes text.
    skipping: bool,
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
            skipping: false,
        }
    }

    /// Decodes `bytes` (one read) into keys, appended to `out`. `whole`
    /// is false when the read filled the caller's buffer, so more bytes
    /// are waiting: a lone ESC at its end may start a sequence (a mouse
    /// report while dragging) and waits for the next read.
    pub fn feed(&mut self, bytes: &[u8], whole: bool, out: &mut Vec<Key>) {
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
                    let (text, cleaned) = pasted_text(&paste[..len]);
                    out.push(Key::Paste { text, cut, cleaned });
                    paste.zeroize();
                }
                continue;
            }
            if self.pending.len() == MAX_PENDING {
                // Nothing this long is a key: drop it, and the rest of it
                // when it is a CSI sequence.
                self.skipping = self.pending.starts_with(b"\x1b[");
                self.clear_pending();
            }
            if self.skipping {
                // ECMA-48: parameter and intermediate bytes go on, a final
                // byte ends the sequence; any other byte (an ESC starting
                // the next sequence) ends it too and is read as usual.
                match byte {
                    0x20..=0x3f => continue,
                    0x40..=0x7e => {
                        self.skipping = false;
                        continue;
                    }
                    _ => self.skipping = false,
                }
            }
            self.pending.push(byte);
            debug_assert!(self.pending.len() <= MAX_PENDING);
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
        // A lone ESC at the end of a whole read is the Esc key: terminals
        // send a whole sequence in one write.
        if whole && self.pending.as_slice() == b"\x1b" {
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
/// reads the bytes in place, so no unwiped copy is made. Also returns
/// whether the cleaning changed anything, for the paste report.
fn pasted_text(bytes: &[u8]) -> (Zeroizing<String>, bool) {
    let mut len = 0;
    let cleaned = clean_paste(bytes, |c| len += c.len_utf8());
    let mut text = Zeroizing::new(String::with_capacity(len));
    clean_paste(bytes, |c| text.push(c));
    (text, cleaned)
}

/// Calls `emit` with each character `pasted_text` keeps; true when it
/// dropped or replaced something.
fn clean_paste(bytes: &[u8], mut emit: impl FnMut(char)) -> bool {
    let mut cleaned = false;
    let mut after_cr = false;
    for chunk in bytes.utf8_chunks() {
        let invalid = (!chunk.invalid().is_empty()).then_some('\u{fffd}');
        cleaned |= invalid.is_some();
        for c in chunk.valid().chars().chain(invalid) {
            let cr = std::mem::replace(&mut after_cr, c == '\r');
            match c {
                '\r' => emit('\n'),
                '\n' if cr => {}
                '\n' | '\t' => emit(c),
                c if crate::ui::grid::is_forbidden(c) => cleaned = true,
                c => emit(c),
            }
        }
    }
    cleaned
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

/// After `ESC [`: parameter and intermediate bytes (0x20 to 0x3f), then
/// one final byte (0x40 to 0x7e), as ECMA-48 defines it, so a sequence
/// not known here is dropped whole. Known ones have digits and `;`; a
/// mouse report has `<` before them.
fn csi(rest: &[u8]) -> Decoded {
    let Some((&last, body)) = rest.split_last() else {
        return Decoded::More;
    };
    match last {
        0x20..=0x3f => return Decoded::More,
        0x40..=0x7e => {}
        // A control byte or beyond ASCII: not a sequence.
        _ => return Decoded::Drop,
    }
    let (report, params) = match body {
        [b'<', params @ ..] => (true, params),
        _ => (false, body),
    };
    if !params.iter().all(|b| b.is_ascii_digit() || *b == b';') {
        return Decoded::Drop;
    }
    if report {
        return mouse(params, last);
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

/// After `ESC [ <`: an SGR mouse report, `button ; column ; row` then
/// `M` (pressed or moved) or `m` (released) (decision 0073). Button 0 is
/// the left one, +32 when moved, 64 and 65 the wheel; any other button,
/// or a modifier (+4, +8, +16), drops the report. Columns and rows count
/// from 1 and have at most 5 digits; anything else drops it.
fn mouse(params: &[u8], last: u8) -> Decoded {
    let mut fields = params.split(|&b| b == b';').map(number);
    let (Some(Some(button)), Some(Some(column)), Some(Some(row)), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Decoded::Drop;
    };
    let event = match (button, last) {
        (0, b'M') => MouseEvent::Press,
        (32, b'M') => MouseEvent::Drag,
        (0, b'm') => MouseEvent::Release,
        (64, b'M') => MouseEvent::WheelUp,
        (65, b'M') => MouseEvent::WheelDown,
        _ => return Decoded::Drop,
    };
    match (column.checked_sub(1), row.checked_sub(1)) {
        (Some(column), Some(row)) => Decoded::Key(Key::Mouse { event, column, row }),
        _ => Decoded::Drop,
    }
}

/// A report's field: 1 to 5 digits (already checked to be digits) that
/// fit in a `u16`.
fn number(field: &[u8]) -> Option<u16> {
    if field.is_empty() || field.len() > 5 {
        return None;
    }
    std::str::from_utf8(field).ok()?.parse().ok()
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
            decoder.feed(chunk, true, &mut out);
        }
        out
    }

    /// Like `keys`, but every read except the last filled the buffer.
    fn keys_in_full_reads(chunks: &[&[u8]]) -> Vec<Key> {
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            decoder.feed(chunk, i + 1 == chunks.len(), &mut out);
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
        // The broken byte makes it a cleaned paste.
        assert!(cleaned(&got[0]));
    }

    fn cleaned(key: &Key) -> bool {
        match key {
            Key::Paste { cleaned, .. } => *cleaned,
            other => panic!("not a paste: {other:?}"),
        }
    }

    // Each kind of cleaning alone marks a paste as cleaned: a C0 control,
    // ESC (what follows it is kept as text), a C1 control, a BiDi override
    // and broken bytes. Line breaks, tabs and other text do not.
    #[test]
    fn cleaned_pastes_are_marked() {
        let got = keys(&[
            b"\x1b[200~a\x07b\x1b[2Jc\xe2\x80\xaed\xc2\x9b",
            b"x\xff\xfee\xc3f\x1b[201~",
        ]);
        assert_eq!(text(&got[0]).unwrap(), "ab[2Jcdx\u{fffd}\u{fffd}e\u{fffd}f");
        assert!(cleaned(&got[0]));
        for one in [&b"\x07"[..], b"\x1b", b"\xc2\x9b", b"\xe2\x80\xae", b"\xff"] {
            let got = keys(&[b"\x1b[200~a", one, b"b\x1b[201~"]);
            assert!(cleaned(&got[0]), "{one:?}");
        }
        let clean = keys(&[b"\x1b[200~plain\ttext\r\n\xc3\xa9\x1b[201~"]);
        assert!(!cleaned(&clean[0]));
    }

    #[test]
    fn long_pastes_are_cut_at_the_limit_and_say_so() {
        let exact = vec![b'a'; MAX_PASTE];
        let got = keys(&[b"\x1b[200~", &exact, b"\x1b[201~"]);
        assert!(
            matches!(&got[..], [Key::Paste { text, cut: false, .. }] if text.len() == MAX_PASTE),
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
                    [Key::Paste { text, cut: true, .. }, Key::Char('z')] if text.len() == MAX_PASTE
                ),
                "{extra}: {got:?}"
            );
        }
    }

    fn mouse(event: MouseEvent, column: u16, row: u16) -> Key {
        Key::Mouse { event, column, row }
    }

    #[test]
    fn mouse_reports() {
        use MouseEvent::*;
        let cases: [(&[u8], MouseEvent, u16, u16); 6] = [
            (b"\x1b[<0;1;1M", Press, 0, 0),
            (b"\x1b[<32;10;3M", Drag, 9, 2),
            (b"\x1b[<0;10;3m", Release, 9, 2),
            (b"\x1b[<64;5;7M", WheelUp, 4, 6),
            (b"\x1b[<65;5;7M", WheelDown, 4, 6),
            (b"\x1b[<0;65535;65535M", Press, 65534, 65534),
        ];
        for (bytes, event, column, row) in cases {
            assert_eq!(keys(&[bytes]), [mouse(event, column, row)], "{bytes:?}");
            // Split anywhere by a full read: the same key.
            for at in 1..bytes.len() {
                let got = keys_in_full_reads(&[&bytes[..at], &bytes[at..]]);
                assert_eq!(got, [mouse(event, column, row)], "{bytes:?} at {at}");
            }
        }
    }

    /// No part of a refused report is ever taken as typed text: the `a`
    /// after it is the only key.
    #[test]
    fn refused_mouse_reports_leave_nothing() {
        let refused: [&[u8]; 17] = [
            b"\x1b[<1;1;1M",      // middle button
            b"\x1b[<2;1;1M",      // right button
            b"\x1b[<3;1;1M",      // no button
            b"\x1b[<4;1;1M",      // Shift
            b"\x1b[<8;1;1M",      // Alt
            b"\x1b[<16;1;1M",     // Ctrl
            b"\x1b[<35;1;1M",     // moved, no button held
            b"\x1b[<32;1;1m",     // a release that moved
            b"\x1b[<0;0;1M",      // column 0
            b"\x1b[<0;1;0M",      // row 0
            b"\x1b[<0;65536;1M",  // past u16
            b"\x1b[<0;123456;1M", // 6 digits
            b"\x1b[<0;1M",        // a field missing
            b"\x1b[<0;1;1;1M",    // a field too many
            b"\x1b[<0;;1M",       // an empty field
            b"\x1b[<0;1;1X",      // another final byte
            b"\x1b[<0;1:1M",      // not a digit
        ];
        for bytes in refused {
            assert_eq!(keys(&[bytes, b"a"]), [Key::Char('a')], "{bytes:?}");
        }
    }

    /// Sequences not known here, and CSI sequences too long to keep,
    /// are dropped whole: the `a` after them is the only key.
    #[test]
    fn unknown_and_long_sequences_leave_nothing() {
        let long = format!("\x1b[{}m", "1;".repeat(40));
        let dropped: [&[u8]; 5] = [
            b"\x1b[?25h",
            b"\x1b[1:2A",
            b"\x1b[ q",
            b"\x1b[>0;1;2c",
            long.as_bytes(),
        ];
        for bytes in dropped {
            assert_eq!(keys(&[bytes, b"a"]), [Key::Char('a')], "{bytes:?}");
            // Split anywhere by a full read: the same.
            for at in 1..bytes.len() {
                let got = keys_in_full_reads(&[&bytes[..at], &bytes[at..], b"a"]);
                assert_eq!(got, [Key::Char('a')], "{bytes:?} at {at}");
            }
        }
    }

    /// A sequence too long to keep, cut short by the next one: the next
    /// one is still read.
    #[test]
    fn a_skipped_sequence_ends_at_the_next_escape() {
        let long = format!("\x1b[{}\x1b[A", "1;".repeat(40));
        assert_eq!(keys(&[long.as_bytes(), b"a"]), [Key::Up, Key::Char('a')]);
    }

    /// A lone ESC ends a whole read as Esc; at the end of a full read it
    /// waits, and a lone ESC then is Esc after the next whole read.
    #[test]
    fn esc_at_the_end_of_a_full_read_waits() {
        assert_eq!(keys(&[b"\x1b"]), [Key::Esc]);
        assert_eq!(keys_in_full_reads(&[b"\x1b", b"[A"]), [Key::Up]);
        assert_eq!(
            keys_in_full_reads(&[b"a\x1b", b""]),
            [Key::Char('a'), Key::Esc]
        );
    }

    #[test]
    fn a_mouse_report_inside_a_paste_is_text() {
        let got = keys(&[b"\x1b[200~x\x1b[<0;1;1My\x1b[201~"]);
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], Key::Paste { .. }));
        // The ESC is dropped by the cleaning, the rest stays text.
        assert_eq!(text(&got[0]).unwrap(), "x[<0;1;1My");
    }

    proptest::proptest! {
        // Reports of any shape, split anywhere: at most one `Mouse` key
        // and nothing else, never text.
        #[test]
        fn mouse_reports_never_become_text(
            button in 0u16..300,
            column in "[0-9]{0,7}",
            row in "[0-9]{0,7}",
            last in proptest::sample::select(vec![b'M', b'm', b'X']),
            at in 0usize..32,
        ) {
            let mut bytes = format!("\x1b[<{button};{column};{row}").into_bytes();
            bytes.push(last);
            let at = at.min(bytes.len());
            let got = keys_in_full_reads(&[&bytes[..at], &bytes[at..]]);
            proptest::prop_assert!(got.len() <= 1, "{got:?}");
            let only_mouse = got.iter().all(|k| matches!(k, Key::Mouse { .. }));
            proptest::prop_assert!(only_mouse, "{got:?}");
        }

        // Text with nothing to clean (no control or BiDi character but `\n`
        // and `\t`, valid UTF-8) arrives unchanged and is not marked.
        #[test]
        fn clean_pastes_report_nothing(input in proptest::prelude::any::<String>()) {
            let clean: String = input
                .chars()
                .filter(|&c| matches!(c, '\n' | '\t') || !crate::ui::grid::is_forbidden(c))
                .collect();
            let got = keys(&[b"\x1b[200~", clean.as_bytes(), b"\x1b[201~"]);
            proptest::prop_assert_eq!(text(&got[0]).unwrap(), clean);
            proptest::prop_assert!(!cleaned(&got[0]));
        }

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
                    decoder.feed(&bytes[at..cut], true, &mut out);
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
