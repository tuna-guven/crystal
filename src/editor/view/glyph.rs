//! One grapheme on screen: how many columns it takes and how it is drawn
//! (step RF1). Every grapheme of the note the editor shows passes through
//! here, as written or in live mode, so this is where decision 0037 is
//! kept for the editor: nothing hidden reaches the terminal.
//!
//! Why each rule exists:
//! - A control or BiDi character, a grapheme of no width, or one that
//!   carries an invisible character (`hides_something`) is spelled out as
//!   a dimmed `<U+XXXX>`: drawn as is, it could reorder or hide the text
//!   around it, and a cursor on it would be invisible.
//! - A grapheme longer than a grid cell holds is spelled out too: the
//!   grid would draw it `?`, one column, while the row walk counted its
//!   full width, so the rows would no longer match (found by the long
//!   tests).
//! - A tab advances to the next multiple of `TAB_STOP` columns, so its
//!   width depends on where it starts: every width here takes the column.
//! - `\r\n` is one line break, as the pager shows it.
//!
//! The escape is written into a stack buffer, never a format string: the
//! characters come from note text, which must not reach `format!`
//! (CLAUDE.md).

use crate::editor::live::Show;
use crate::ui::grid::{
    Grid, MAX_CELL_BYTES, Mods, Style, grapheme_width, hides_something, is_forbidden,
};

/// A tab advances to the next multiple of this many columns.
pub const TAB_STOP: usize = 4;
/// `<U+`, up to 6 hex digits, `>`.
pub const ESCAPE_LEN: usize = 10;

/// How one grapheme is drawn.
pub enum Shape<'a> {
    /// `\n`, or `\r\n` (shown as one line break, as the pager does).
    Break,
    Tab,
    /// Drawn as is, this many columns wide.
    Text(&'a str, usize),
    /// Spelled out, each character as `<U+XXXX>`.
    Escape,
}

/// Sorts `grapheme` into one of the shapes above.
pub fn shape(grapheme: &str) -> Shape<'_> {
    match grapheme {
        "\n" | "\r\n" => Shape::Break,
        "\t" => Shape::Tab,
        _ => match usize::from(grapheme_width(grapheme)) {
            // Longer than a grid cell holds, it would be drawn `?`, one
            // column, while laid out as `width` (found by the long tests).
            width
                if width > 0
                    && grapheme.len() <= MAX_CELL_BYTES
                    && !grapheme.contains(is_forbidden)
                    && !hides_something(grapheme) =>
            {
                Shape::Text(grapheme, width)
            }
            _ => Shape::Escape,
        },
    }
}

/// `c` as `<U+XXXX>` (4 to 6 hex digits), written into `buf`: no
/// allocation and no format string for note text.
pub fn escape(c: char, buf: &mut [u8; ESCAPE_LEN]) -> &str {
    let code = u32::from(c);
    let digits = (32 - code.leading_zeros()).div_ceil(4).max(4) as usize;
    buf[..3].copy_from_slice(b"<U+");
    for i in 0..digits {
        let nibble = (code >> (4 * (digits - 1 - i))) & 0xf;
        buf[3 + i] = b"0123456789ABCDEF"[nibble as usize];
    }
    buf[3 + digits] = b'>';
    std::str::from_utf8(&buf[..digits + 4]).unwrap_or("?")
}

/// The columns `grapheme` takes spelled out: each of its characters'
/// escapes, side by side.
fn escape_width(grapheme: &str) -> usize {
    let mut buf = [0; ESCAPE_LEN];
    grapheme.chars().map(|c| escape(c, &mut buf).len()).sum()
}

/// The columns `grapheme` takes when it starts at `column`.
pub fn width_at(grapheme: &str, column: usize) -> usize {
    match shape(grapheme) {
        Shape::Break => 0,
        Shape::Tab => TAB_STOP - column % TAB_STOP,
        Shape::Text(_, width) => width,
        Shape::Escape => escape_width(grapheme),
    }
}

/// Draws `grapheme`, which starts `column` columns into its row, at
/// `x, y` with `room` columns left, as `show` says: hidden markup not at
/// all, a fixed string in its place, a tab as spaces, an invisible
/// character spelled out dimmed. `link`, a web address, goes on its
/// cells for the terminal (Ctrl+click), as in the viewer; the grid
/// checks the address again. Returns the columns it took.
#[allow(clippy::too_many_arguments)]
pub fn put(
    grid: &mut Grid,
    (x, y, room): (u16, u16, u16),
    grapheme: &str,
    column: usize,
    style: Style,
    show: Show,
    link: Option<&str>,
) -> usize {
    match show {
        Show::Hidden => return 0,
        // A rule's line to the row's end, as the viewer's: the text's
        // width, not the column kept for the cursor.
        Show::Rule => {
            let columns = room.saturating_sub(1).max(1);
            for i in 0..columns {
                grid.put_str(x + i, y, "─", style, 1);
            }
            return usize::from(columns);
        }
        Show::Text => {}
    }
    match shape(grapheme) {
        Shape::Text(g, _) => {
            let used = grid.put_str(x, y, g, style, room);
            if let Some(target) = link {
                let id = grid.add_link(target);
                grid.set_link(x, y, used, id);
            }
        }
        Shape::Tab => {
            let w = width_at(grapheme, column);
            grid.put_str(x, y, &"    "[..w], style, room);
        }
        Shape::Escape => {
            let dim = style.mods(style.mods.union(Mods::DIM));
            let mut buf = [0; ESCAPE_LEN];
            let mut used = 0;
            for c in grapheme.chars() {
                let left = room.saturating_sub(used);
                used += grid.put_str(x + used, y, escape(c, &mut buf), dim, left);
            }
        }
        Shape::Break => {}
    }
    width_at(grapheme, column)
}
