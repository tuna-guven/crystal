//! Crystal's own terminal backend (decision 0031): turns the
//! difference between two grids into the bytes that update the screen.
//!
//! - Only cells that changed are written (`Grid::diff`).
//! - Every cell's symbol is checked once more right before it is written.
//!   `Grid::put_str` already replaces forbidden characters, so this is a
//!   last line of defence: a symbol with a control or BiDi character is
//!   drawn as `?`, padded to the cell's width.
//! - Output goes through one fixed-size chunk that is wiped after every
//!   flush: frames hold note text, and a growing `Vec` would leave unwiped
//!   copies behind on every reallocation.
//! - The only sequences Crystal writes itself are cursor moves (CUP) and
//!   styles (SGR), built from numbers, and hyperlinks (OSC 8,
//!   decision 0054): a link's target is written
//!   only if `md::link::clickable` accepts it, checked again here; it is
//!   printable ASCII without ESC or BEL, so it cannot end or extend the
//!   sequence. A link is opened where its cells start and closed where
//!   they end, and always before the frame ends.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the viewer loop (Phase 3, V6)")
)]

use std::io::{self, Write};

use zeroize::{Zeroize, Zeroizing};

use super::grid::{Color, Grid, Mods, REPLACEMENT, Style, is_forbidden};
use crate::md::link::clickable;

/// Bytes buffered before a write to the terminal.
const CHUNK: usize = 4096;

/// All attributes off, default colors.
const RESET_STYLE: &[u8] = b"\x1b[0m";

/// Starts a hyperlink (OSC 8, no parameters): the target, then `LINK_END`.
const LINK_START: &[u8] = b"\x1b]8;;";
/// Ends the OSC 8 sequence (ST).
const LINK_END: &[u8] = b"\x1b\\";

/// Buffers terminal output in one fixed, zeroizing chunk and hands it to
/// `out` when full and at `flush`. It never allocates.
pub struct FrameWriter<W: Write> {
    out: W,
    chunk: Zeroizing<[u8; CHUNK]>,
    len: usize,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            chunk: Zeroizing::new([0; CHUNK]),
            len: 0,
        }
    }

    /// Writes the buffered bytes to `out` and wipes them, also when the
    /// write fails.
    fn drain(&mut self) -> io::Result<()> {
        let result = self.out.write_all(&self.chunk[..self.len]);
        self.chunk[..self.len].zeroize();
        self.len = 0;
        result
    }
}

impl<W: Write> Write for FrameWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.len == CHUNK {
            self.drain()?;
        }
        let n = bytes.len().min(CHUNK - self.len);
        self.chunk[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.out.flush()
    }
}

impl<W: Write> Drop for FrameWriter<W> {
    fn drop(&mut self) {
        self.chunk.zeroize();
    }
}

/// Writes the changes from `shown` (what the terminal shows) to `next`
/// and flushes them. Grids of different sizes are written whole. The
/// style is reset at the end, so later output (a prompt) starts plain.
pub fn draw<W: Write>(shown: &Grid, next: &Grid, out: &mut FrameWriter<W>) -> io::Result<()> {
    let right = next.width();
    // Where the terminal's cursor is, if known; unknown at the start and
    // after the last column (terminals differ on the pending wrap).
    let mut cursor: Option<(u16, u16)> = None;
    out.write_all(RESET_STYLE)?;
    let mut style = Style::PLAIN;
    // The link open on the terminal, by target.
    let mut open: Option<&str> = None;
    for (x, y, cell) in next.diff(shown) {
        if cursor != Some((x, y)) {
            // CUP is 1-based.
            write!(out, "\x1b[{};{}H", u32::from(y) + 1, u32::from(x) + 1)?;
        }
        let link = next.link_target(cell.link()).filter(|t| clickable(t));
        if link != open {
            if open.is_some() {
                write_link(out, "")?;
            }
            if let Some(target) = link {
                write_link(out, target)?;
            }
            open = link;
        }
        if cell.style() != style {
            style = cell.style();
            write_style(out, style)?;
        }
        let symbol = cell.symbol();
        if symbol.contains(is_forbidden) {
            out.write_all(REPLACEMENT.as_bytes())?;
            for _ in 1..cell.width() {
                out.write_all(b" ")?;
            }
        } else {
            out.write_all(symbol.as_bytes())?;
        }
        let next_x = x.saturating_add(u16::from(cell.width()));
        cursor = (next_x < right).then_some((next_x, y));
    }
    if open.is_some() {
        write_link(out, "")?;
    }
    out.write_all(RESET_STYLE)?;
    out.flush()
}

/// One OSC 8 sequence: opens a link to `target`, or closes the open one
/// when `target` is empty. `target` is empty or `clickable`.
fn write_link<W: Write>(out: &mut FrameWriter<W>, target: &str) -> io::Result<()> {
    out.write_all(LINK_START)?;
    out.write_all(target.as_bytes())?;
    out.write_all(LINK_END)
}

/// One SGR sequence: reset, then the attributes, then the colors.
fn write_style<W: Write>(out: &mut FrameWriter<W>, style: Style) -> io::Result<()> {
    const MODS: [(Mods, &[u8]); 6] = [
        (Mods::BOLD, b";1"),
        (Mods::DIM, b";2"),
        (Mods::ITALIC, b";3"),
        (Mods::UNDERLINED, b";4"),
        (Mods::REVERSED, b";7"),
        (Mods::CROSSED_OUT, b";9"),
    ];
    out.write_all(b"\x1b[0")?;
    for (mods, code) in MODS {
        if style.mods.contains(mods) {
            out.write_all(code)?;
        }
    }
    write_color(out, style.fg, 0)?;
    write_color(out, style.bg, 10)?;
    out.write_all(b"m")
}

/// `;N` for a color, nothing for the default (already set by the reset).
/// `layer` is 0 for the foreground, 10 for the background.
fn write_color<W: Write>(out: &mut FrameWriter<W>, color: Color, layer: u8) -> io::Result<()> {
    let code = match color {
        Color::Default => return Ok(()),
        Color::Indexed(index) => return write!(out, ";{};5;{index}", 38 + layer),
        Color::Black => 30,
        Color::Red => 31,
        Color::Green => 32,
        Color::Yellow => 33,
        Color::Blue => 34,
        Color::Magenta => 35,
        Color::Cyan => 36,
        Color::Gray => 37,
        Color::DarkGray => 90,
        Color::LightRed => 91,
        Color::LightGreen => 92,
        Color::LightYellow => 93,
        Color::LightBlue => 94,
        Color::LightMagenta => 95,
        Color::LightCyan => 96,
        Color::White => 97,
    };
    write!(out, ";{}", code + layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid with `lines` written from the top left.
    fn grid(width: u16, height: u16, lines: &[&str]) -> Grid {
        let mut grid = Grid::new(width, height);
        for (y, line) in (0u16..).zip(lines) {
            grid.put_str(0, y, line, Style::PLAIN, width);
        }
        grid
    }

    /// The bytes `draw` writes for `shown` to `next`.
    fn frame(shown: &Grid, next: &Grid) -> Vec<u8> {
        let mut bytes = Vec::new();
        draw(shown, next, &mut FrameWriter::new(&mut bytes)).unwrap();
        bytes
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn unchanged_grid_writes_only_resets() {
        let a = grid(3, 2, &["abc", "def"]);
        let b = grid(3, 2, &["abc", "def"]);
        assert_eq!(text(&frame(&a, &b)), "\x1b[0m\x1b[0m");
    }

    #[test]
    fn writes_changed_cells_with_one_move_per_run() {
        let shown = Grid::new(5, 2);
        let next = grid(5, 2, &["ab", "   cd"]);
        assert_eq!(
            text(&frame(&shown, &next)),
            "\x1b[0m\x1b[1;1Hab\x1b[2;4Hcd\x1b[0m"
        );
    }

    #[test]
    fn styles_change_only_when_needed() {
        let shown = Grid::new(4, 1);
        let mut next = Grid::new(4, 1);
        let bold_red = Style::PLAIN
            .fg(Color::Red)
            .bg(Color::Indexed(236))
            .mods(Mods::BOLD | Mods::UNDERLINED);
        next.put_str(0, 0, "ab", bold_red, 2);
        next.put_str(2, 0, "c", Style::PLAIN.fg(Color::Indexed(7)), 1);
        assert_eq!(
            text(&frame(&shown, &next)),
            "\x1b[0m\x1b[1;1H\x1b[0;1;4;31;48;5;236mab\x1b[0;38;5;7mc\x1b[0m"
        );
    }

    #[test]
    fn every_attribute_has_its_code() {
        let cases = [
            (Mods::BOLD, 1),
            (Mods::DIM, 2),
            (Mods::ITALIC, 3),
            (Mods::UNDERLINED, 4),
            (Mods::REVERSED, 7),
            (Mods::CROSSED_OUT, 9),
        ];
        for (mods, code) in cases {
            let mut next = Grid::new(1, 1);
            next.put_str(0, 0, "x", Style::PLAIN.mods(mods), 1);
            let expected = format!("\x1b[0m\x1b[1;1H\x1b[0;{code}mx\x1b[0m");
            assert_eq!(text(&frame(&Grid::new(1, 1), &next)), expected);
        }
    }

    #[test]
    fn every_named_color_has_its_code() {
        let cases = [
            (Color::Black, 30),
            (Color::Red, 31),
            (Color::Green, 32),
            (Color::Yellow, 33),
            (Color::Blue, 34),
            (Color::Magenta, 35),
            (Color::Cyan, 36),
            (Color::Gray, 37),
            (Color::DarkGray, 90),
            (Color::LightRed, 91),
            (Color::LightGreen, 92),
            (Color::LightYellow, 93),
            (Color::LightBlue, 94),
            (Color::LightMagenta, 95),
            (Color::LightCyan, 96),
            (Color::White, 97),
        ];
        for (color, code) in cases {
            let mut next = Grid::new(1, 1);
            next.put_str(0, 0, "x", Style::PLAIN.fg(color).bg(color), 1);
            let expected = format!("\x1b[0m\x1b[1;1H\x1b[0;{code};{}mx\x1b[0m", code + 10);
            assert_eq!(text(&frame(&Grid::new(1, 1), &next)), expected, "{color:?}");
        }
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let next = grid(4, 1, &["日x"]);
        // One move: after the wide grapheme the cursor is at column 2,
        // where `x` goes.
        assert_eq!(
            text(&frame(&Grid::new(4, 1), &next)),
            "\x1b[0m\x1b[1;1H日x\x1b[0m"
        );
    }

    #[test]
    fn last_column_forces_a_move() {
        let next = grid(2, 2, &["ab", "cd"]);
        assert_eq!(
            text(&frame(&Grid::new(2, 2), &next)),
            "\x1b[0m\x1b[1;1Hab\x1b[2;1Hcd\x1b[0m"
        );
    }

    #[test]
    fn different_sizes_are_drawn_whole() {
        let next = grid(2, 1, &["ab"]);
        assert_eq!(
            text(&frame(&Grid::new(5, 5), &next)),
            "\x1b[0m\x1b[1;1Hab\x1b[0m"
        );
    }

    #[test]
    fn escape_sequences_in_text_arrive_inert() {
        // ESC, CSI as C1, OSC 52 and OSC 8, BEL, a BiDi override: the only
        // ESC bytes written are Crystal's own (reset, move, reset).
        let hostile = "a\u{1b}[2J\u{9b}31m\u{1b}]52;c;aGk=\u{7}\u{1b}]8;;x\u{1b}\\\u{202E}z";
        let next = grid(60, 1, &[hostile]);
        let bytes = frame(&Grid::new(60, 1), &next);
        assert_eq!(bytes.iter().filter(|&&b| b == 0x1b).count(), 3);
        let shown = text(&bytes);
        assert!(
            !shown.contains(|c| c != '\u{1b}' && is_forbidden(c)),
            "{shown:?}"
        );
        assert!(shown.contains("a?[2J?31m"), "{shown:?}");
    }

    /// A grid with `text` on its one row and `link` set on columns
    /// `from..to`.
    fn linked(text: &str, target: &str, from: u16, to: u16) -> Grid {
        let mut g = grid(8, 1, &[text]);
        let link = g.add_link(target);
        g.set_link(from, 0, to - from, link);
        g
    }

    #[test]
    fn links_open_where_they_start_and_close_where_they_end() {
        let next = linked("a link b", "https://e.org/x", 2, 6);
        assert_eq!(
            text(&frame(&Grid::new(8, 1), &next)),
            // Spaces already on the blank screen are skipped.
            "\x1b[0m\x1b[1;1Ha\x1b[1;3H\x1b]8;;https://e.org/x\x1b\\link\
             \x1b[1;8H\x1b]8;;\x1b\\b\x1b[0m"
        );
        // A link at the end of the frame is closed before it ends.
        let next = linked("a link", "mailto:m@e.org", 2, 6);
        assert!(
            text(&frame(&Grid::new(8, 1), &next))
                .ends_with("\x1b]8;;mailto:m@e.org\x1b\\link\x1b]8;;\x1b\\\x1b[0m")
        );
        // Two links side by side: the first closes, the second opens.
        let mut next = grid(8, 1, &["abcd"]);
        let (a, b) = (
            next.add_link("https://a.org"),
            next.add_link("https://b.org"),
        );
        next.set_link(0, 0, 2, a);
        next.set_link(2, 0, 2, b);
        assert_eq!(
            text(&frame(&Grid::new(8, 1), &next)),
            "\x1b[0m\x1b[1;1H\x1b]8;;https://a.org\x1b\\ab\x1b]8;;\x1b\\\
             \x1b]8;;https://b.org\x1b\\cd\x1b]8;;\x1b\\\x1b[0m"
        );
    }

    /// A target the grid should never hold (forced in here) is not
    /// written: its cells are plain text.
    #[test]
    fn unsafe_link_target_is_not_written() {
        for target in [
            "https://e.org/\u{1b}\\",
            "file:///etc/passwd",
            "https://e.org/\u{7}x",
        ] {
            let mut next = grid(4, 1, &["link"]);
            let link = next.push_link_raw_for_test(target);
            next.set_link(0, 0, 4, link);
            assert_eq!(
                text(&frame(&Grid::new(4, 1), &next)),
                "\x1b[0m\x1b[1;1Hlink\x1b[0m",
                "{target:?}"
            );
        }
    }

    proptest::proptest! {
        // Whatever the targets and wherever they are set, every OSC 8 the
        // frame writes carries a clickable target or none, opens and
        // closes alternate, and the frame ends with none open.
        #[test]
        fn written_links_are_always_safe_and_closed(
            targets in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::any::<String>(),
                    "https://[a-z]{1,8}\\.org/[ -~]{0,12}",
                ],
                1..5,
            ),
            raw in proptest::prelude::any::<bool>(),
            spans in proptest::collection::vec((0u16..12, 0u16..3, 0u16..6, 0usize..5), 0..8),
        ) {
            let mut next = grid(12, 3, &["0123456789ab", "cdefghijklmn", "opqrstuvwxyz"]);
            let links: Vec<u16> = targets
                .iter()
                .map(|t| if raw { next.push_link_raw_for_test(t) } else { next.add_link(t) })
                .collect();
            for (x, y, len, which) in spans {
                next.set_link(x, y, len, links[which % links.len()]);
            }
            let bytes = frame(&Grid::new(12, 3), &next);
            let mut rest = &bytes[..];
            let mut open = false;
            while let Some(at) = rest.windows(LINK_START.len()).position(|w| w == LINK_START) {
                let after = &rest[at + LINK_START.len()..];
                let end = after.windows(2).position(|w| w == LINK_END).unwrap();
                let target = std::str::from_utf8(&after[..end]).unwrap();
                if target.is_empty() {
                    proptest::prop_assert!(open, "a close without an open");
                } else {
                    proptest::prop_assert!(!open, "an open inside an open");
                    proptest::prop_assert!(clickable(target), "{target:?}");
                }
                open = !target.is_empty();
                rest = &after[end + 2..];
            }
            proptest::prop_assert!(!open, "left open");
        }
    }

    #[test]
    fn forbidden_symbol_in_a_cell_is_replaced_at_its_width() {
        // put_str never stores one; a cell is forced here to test the
        // last line of defence.
        let mut next = Grid::new(4, 1);
        next.put_raw_for_test(0, 0, "\u{1b}", 2);
        next.put_str(2, 0, "z", Style::PLAIN, 1);
        assert_eq!(
            text(&frame(&Grid::new(4, 1), &next)),
            "\x1b[0m\x1b[1;1H? z\x1b[0m"
        );
    }

    #[test]
    fn output_larger_than_a_chunk_arrives_whole() {
        let line = "x".repeat(200);
        let next = grid(200, 60, &vec![line.as_str(); 60]);
        let bytes = frame(&Grid::new(200, 60), &next);
        assert!(bytes.len() > CHUNK);
        assert_eq!(bytes.iter().filter(|&&b| b == b'x').count(), 200 * 60);
        assert!(bytes.ends_with(RESET_STYLE));
    }

    #[test]
    fn writer_wipes_its_chunk_after_each_flush() {
        let mut bytes = Vec::new();
        let mut out = FrameWriter::new(&mut bytes);
        out.write_all(b"secret note text").unwrap();
        out.flush().unwrap();
        assert!(out.chunk.iter().all(|&b| b == 0));
        drop(out);
        assert_eq!(bytes, b"secret note text");
    }

    /// A terminal that refuses writes.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_write_still_wipes_the_chunk() {
        let mut out = FrameWriter::new(Broken);
        out.write_all(b"secret note text").unwrap();
        assert!(out.flush().is_err());
        assert!(out.chunk.iter().all(|&b| b == 0));
        assert_eq!(out.len, 0);
    }
}
