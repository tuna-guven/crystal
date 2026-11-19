//! The screen: two grids, the one on the terminal and the one being
//! drawn, and the step that sends the difference (decision 0031).
//!
//! The first frame, and the first after a resize or a resume, clears the
//! screen and hides the cursor, then draws onto a blank buffer, so every
//! cell is written once. Later frames write only what changed.
//!
//! Mouse reports are on exactly while the frame asks for them (the
//! editor, decision 0073): the screen writes the change, and a full
//! redraw starts with them off.
//!
//! Both grids hold note text; they are wiped on every reset and on drop.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the viewer loop (Phase 3, V6)")
)]

use std::io::{self, Write};
use std::mem;

use super::frame::{FrameWriter, draw};
use super::grid::Grid;
use crate::term::{MOTION_ON, MOUSE_OFF, MOUSE_ON};

/// Close any link a broken-off frame left open (`ui::frame`), mouse
/// reports off (`term::MOUSE_OFF`; only the editor's and the viewer's
/// frames turn them on, decisions 0073 and 0076), hide the cursor, turn on bracketed paste (pasted text arrives marked, never as
/// keys: `shell::keys`), clear the screen, cursor to top left. Again after
/// every resume, since leaving the screen turns paste off.
const FULL_REDRAW: &[u8] =
    b"\x1b]8;;\x1b\\\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?25l\x1b[?2004h\x1b[2J\x1b[H";

/// Which mouse reports a frame asks the terminal for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    Off,
    /// Presses, releases, drags and the wheel: the editor (decision 0073).
    Buttons,
    /// Those and every motion, no button held too: the viewer (decision
    /// 0076).
    Motion,
}

pub struct Screen {
    /// What the terminal shows.
    shown: Grid,
    /// What the next frame draws into.
    next: Grid,
    /// Clear and draw everything on the next frame.
    full: bool,
    /// Which mouse reports the terminal sends, as last written.
    mouse: Mouse,
}

impl Screen {
    /// A screen of `columns` by `rows`; the first frame is a full redraw.
    pub fn new((columns, rows): (u16, u16)) -> Self {
        Self {
            shown: Grid::new(columns, rows),
            next: Grid::new(columns, rows),
            full: true,
            mouse: Mouse::Off,
        }
    }

    /// After a resize or a resume: new buffers of the new size, and a full
    /// redraw on the next frame.
    pub fn redraw_all(&mut self, size: (u16, u16)) {
        *self = Self::new(size);
    }

    /// The size widgets draw into, as (columns, rows).
    pub fn size(&self) -> (u16, u16) {
        (self.next.width(), self.next.height())
    }

    /// Lets `render` fill a blank grid, then writes the difference to
    /// `out` and flushes it. Mouse reports are set to `mouse`, written only
    /// when that changes: off first, then the new kind on.
    pub fn frame<W: Write>(
        &mut self,
        out: W,
        mouse: Mouse,
        render: impl FnOnce(&mut Grid),
    ) -> io::Result<()> {
        self.next.reset();
        render(&mut self.next);
        let mut out = FrameWriter::new(out);
        // Until this frame is written, assume a full redraw is needed: if
        // a write fails, the terminal may show any mix of two frames.
        if mem::replace(&mut self.full, true) {
            out.write_all(FULL_REDRAW)?;
            self.shown.reset();
            self.mouse = Mouse::Off;
        }
        // Recorded only once written; a failed write leaves `full` set,
        // and the next frame's `FULL_REDRAW` turns reports off first.
        if mouse != self.mouse {
            if self.mouse != Mouse::Off {
                out.write_all(MOUSE_OFF)?;
            }
            match mouse {
                Mouse::Off => {}
                Mouse::Buttons => out.write_all(MOUSE_ON)?,
                Mouse::Motion => out.write_all(MOTION_ON)?,
            }
            self.mouse = mouse;
        }
        draw(&self.shown, &self.next, &mut out)?;
        self.full = false;
        mem::swap(&mut self.shown, &mut self.next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::grid::Style;

    fn frame(screen: &mut Screen, lines: &[&str]) -> String {
        frame_with(screen, lines, Mouse::Off)
    }

    fn frame_with(screen: &mut Screen, lines: &[&str], mouse: Mouse) -> String {
        let mut bytes = Vec::new();
        screen
            .frame(&mut bytes, mouse, |grid| {
                for (y, line) in (0u16..).zip(lines) {
                    grid.put_str(0, y, line, Style::PLAIN, u16::MAX);
                }
            })
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn first_frame_clears_and_draws_everything() {
        let mut screen = Screen::new((3, 2));
        assert_eq!(
            frame(&mut screen, &["ab", "c"]),
            "\x1b]8;;\x1b\\\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?25l\x1b[?2004h\x1b[2J\x1b[H\x1b[0m\x1b[1;1Hab\x1b[2;1Hc\x1b[0m"
        );
    }

    /// Mouse reports go on with the first frame that asks for them and off
    /// with the first that does not, written once each; a full redraw
    /// turns them off, and the same frame turns them on again if asked
    /// (decision 0073).
    #[test]
    fn mouse_reports_follow_the_frames() {
        let on = std::str::from_utf8(MOUSE_ON).unwrap();
        let off = std::str::from_utf8(MOUSE_OFF).unwrap();
        let mut screen = Screen::new((3, 1));
        let motion = std::str::from_utf8(MOTION_ON).unwrap();
        let first = frame_with(&mut screen, &["ab"], Mouse::Buttons);
        assert!(
            first.find(off).unwrap() < first.find(on).unwrap(),
            "{first:?}"
        );
        assert_eq!(
            frame_with(&mut screen, &["ab"], Mouse::Buttons),
            "\x1b[0m\x1b[0m"
        );
        assert_eq!(
            frame_with(&mut screen, &["ab"], Mouse::Off),
            format!("{off}\x1b[0m\x1b[0m")
        );
        assert_eq!(
            frame_with(&mut screen, &["ab"], Mouse::Off),
            "\x1b[0m\x1b[0m"
        );
        assert_eq!(
            frame_with(&mut screen, &["ab"], Mouse::Buttons),
            format!("{on}\x1b[0m\x1b[0m")
        );
        // From one kind to the other: off first, then the other on.
        assert_eq!(
            frame_with(&mut screen, &["ab"], Mouse::Motion),
            format!("{off}{motion}\x1b[0m\x1b[0m")
        );
        assert_eq!(
            frame_with(&mut screen, &["ab"], Mouse::Off),
            format!("{off}\x1b[0m\x1b[0m")
        );
        screen.redraw_all((3, 1));
        let redrawn = frame_with(&mut screen, &["ab"], Mouse::Off);
        assert!(
            redrawn.contains(off) && !redrawn.contains(on) && !redrawn.contains(motion),
            "{redrawn:?}"
        );
        // A failed frame: the next one starts over with reports off.
        assert!(screen.frame(Broken, Mouse::Motion, |_| {}).is_err());
        let after = frame_with(&mut screen, &["ab"], Mouse::Off);
        assert!(after.contains(off) && !after.contains(motion), "{after:?}");
    }

    #[test]
    fn full_redraws_turn_mouse_reports_off() {
        let off = crate::term::MOUSE_OFF;
        assert!(FULL_REDRAW.windows(off.len()).any(|w| w == off));
    }

    #[test]
    fn later_frames_write_only_changes() {
        let mut screen = Screen::new((3, 2));
        frame(&mut screen, &["ab", "c"]);
        assert_eq!(frame(&mut screen, &["ab", "c"]), "\x1b[0m\x1b[0m");
        assert_eq!(frame(&mut screen, &["ax", "c"]), "\x1b[0m\x1b[1;2Hx\x1b[0m");
        // Text that disappears is overwritten with blanks.
        assert_eq!(frame(&mut screen, &["ax"]), "\x1b[0m\x1b[2;1H \x1b[0m");
    }

    #[test]
    fn redraw_all_starts_over_at_the_new_size() {
        let mut screen = Screen::new((3, 2));
        frame(&mut screen, &["ab", "c"]);
        screen.redraw_all((2, 1));
        assert_eq!(screen.size(), (2, 1));
        assert_eq!(
            frame(&mut screen, &["ab"]),
            "\x1b]8;;\x1b\\\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?25l\x1b[?2004h\x1b[2J\x1b[H\x1b[0m\x1b[1;1Hab\x1b[0m"
        );
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
    fn failed_frame_is_followed_by_a_full_redraw() {
        let mut screen = Screen::new((3, 1));
        frame(&mut screen, &["ab"]);
        assert!(screen.frame(Broken, Mouse::Buttons, |_| {}).is_err());
        assert!(screen.full);
        assert!(frame(&mut screen, &["ab"]).starts_with(
            "\x1b]8;;\x1b\\\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?25l\x1b[?2004h\x1b[2J"
        ));
    }
}
