//! The Crystal shell (docs/decisions/0033-shell.md): a prompt on the bottom
//! row, command output scrolling above it, all on the alternate screen and
//! in memory only. The scrollback, the history and the line being typed
//! are zeroizing and are dropped (wiped) on lock and exit.
//!
//! This module is terminal-free: keys go in (`key`), a frame comes out
//! (`draw`), so it is tested without a pty. Commands that need the vault
//! are returned to the caller as `Action::Command`.

pub mod complete;
pub mod keys;
pub mod line;
pub mod parse;
pub mod scrollback;

use std::collections::VecDeque;

use unicode_segmentation::UnicodeSegmentation;
use zeroize::Zeroizing;

use crate::ui::grid::{Color, Grid, Mods, Style, display_width};
use crate::vault::VaultName;
use keys::Key;
use line::Line;
use scrollback::Scrollback;

/// The most commands remembered.
pub const MAX_HISTORY: usize = 200;

const PROMPT_STYLE: Style = Style::PLAIN.fg(Color::Cyan).mods(Mods::BOLD);
const ECHO_STYLE: Style = Style::PLAIN.mods(Mods::DIM);
pub const ERROR_STYLE: Style = Style::PLAIN.fg(Color::Red);

/// A paste longer than the prompt line (or than any paste may be) was cut
/// (docs/decisions/0045-simple-editor.md).
const PASTE_CUT: &str = "The paste was cut: it was longer than the line holds.";
const CURSOR_STYLE: Style = Style::PLAIN.mods(Mods::REVERSED);

pub const HELP: &str = "\
Commands:
  ls [FOLDER]   notes and folders as a tree
  ls #TAG       the notes with a tag
  tags          every tag, with how many notes have it
  find TEXT     names that hold TEXT's letters in order, best first
  search TEXT   notes whose name or text holds TEXT (ignoring case)
  view NAME     the note, full screen: Tab and Enter follow [[links]],
                Backspace goes back, b lists backlinks, q returns here
  edit NAME     the note in the editor: Ctrl+S saves, Ctrl+E shows it
                rendered, Ctrl+Q closes
  new NAME      a new note (new FOLDER/NAME in a folder), in the editor;
                it exists once Ctrl+S saves it
  mv OLD NEW    rename or move a note; [[links]] to it follow
  rm NAME       delete a note for good, after you type y
  links         every [[link]] that leads nowhere
  help          this list
  clear         clear the screen (Ctrl+L)
  quit          lock the vault and leave (also exit, or Ctrl+D on an empty line)
Names with spaces go in quotes: search \"two words\".
Keys: Tab completes commands and names; arrows, Home and End move; Ctrl+U, Ctrl+K and Ctrl+W delete; Up and Down
recall commands; PgUp and PgDn scroll; Ctrl+C clears the line.";

/// What the caller should do after a key.
pub enum Action {
    Nothing,
    Quit,
    /// A command for the caller: its words (the first is the command).
    Command(Vec<Zeroizing<String>>),
}

pub struct Shell {
    /// `crystal:NAME> `: the vault name is validated ASCII, not note text.
    prompt: String,
    line: Line,
    output: Scrollback,
    history: VecDeque<Zeroizing<String>>,
    /// While walking the history: the entry shown, and the line that was
    /// being typed before.
    recalled: Option<(usize, Zeroizing<String>)>,
    /// Lines scrolled up from the bottom.
    scroll: usize,
    /// Rows of output last drawn, for PgUp and PgDn.
    page: usize,
    /// Note names, for completion.
    names: Vec<Zeroizing<String>>,
    /// The last key was a Tab that could not complete further: the next
    /// one lists the options.
    tabbed: bool,
}

impl Shell {
    pub fn new(vault: &VaultName) -> Self {
        Self {
            prompt: format!("crystal:{}> ", vault.as_str()),
            line: Line::new(),
            output: Scrollback::new(),
            history: VecDeque::new(),
            recalled: None,
            scroll: 0,
            page: 1,
            names: Vec::new(),
            tabbed: false,
        }
    }

    /// The note names Tab completes after a command.
    pub fn set_names(&mut self, names: Vec<Zeroizing<String>>) {
        self.names = names;
    }

    /// Tab: complete the word before the cursor (`complete`).
    fn complete(&mut self) {
        let word = complete::word_at(self.line.as_str(), self.line.cursor());
        let options: Vec<&str> = if word.first {
            complete::options(&word.text, complete::COMMANDS.iter().copied())
        } else {
            complete::options(&word.text, self.names.iter().map(|n| n.as_str()))
        };
        let replacement = match options.as_slice() {
            [] => return,
            [one] => {
                let mut done = complete::quoted(one, true);
                if !one.ends_with('/') {
                    done.push(' ');
                }
                done
            }
            several => {
                let shared = complete::shared_prefix(several);
                if shared.chars().count() > word.text.chars().count() {
                    complete::quoted(shared, false)
                } else {
                    if self.tabbed {
                        let shown = several.iter().take(50);
                        let mut list = Zeroizing::new(String::with_capacity(
                            shown.clone().map(|o| o.len() + 3).sum(),
                        ));
                        for option in shown {
                            list.push_str(option);
                            list.push_str("   ");
                        }
                        self.print(&list, ECHO_STYLE);
                    }
                    self.tabbed = true;
                    return;
                }
            }
        };
        self.line.replace_before_cursor(word.start, &replacement);
    }

    /// How many output entries there are (tests).
    #[cfg(test)]
    pub fn output_len(&self) -> usize {
        self.output.len()
    }

    /// Adds output (sanitized by the scrollback).
    pub fn print(&mut self, text: &str, style: Style) {
        self.output.push(text, style);
        self.scroll = 0;
    }

    pub fn key(&mut self, key: Key) -> Action {
        let key = key.without_shift();
        if key != Key::Tab {
            self.tabbed = false;
        }
        if !matches!(key, Key::PageUp | Key::PageDown) {
            self.scroll = 0;
        }
        match key {
            Key::Char(c) => {
                let mut buf = [0u8; 4];
                self.line.insert(c.encode_utf8(&mut buf));
            }
            Key::Paste { text, cut } => {
                let fits = self.line.insert(&keys::one_line(&text));
                if cut || !fits {
                    self.print(PASTE_CUT, ERROR_STYLE);
                }
            }
            Key::Enter => return self.submit(),
            Key::Backspace => self.line.backspace(),
            Key::Delete => self.line.delete(),
            Key::Left => self.line.left(),
            Key::Right => self.line.right(),
            Key::Home | Key::Ctrl('a') => self.line.home(),
            Key::End | Key::Ctrl('e') => self.line.end(),
            Key::Ctrl('u') => self.line.kill_before(),
            Key::Ctrl('k') => self.line.kill_after(),
            Key::Ctrl('w') => self.line.kill_word(),
            Key::Ctrl('c') => {
                self.line.clear();
                self.recalled = None;
            }
            Key::Ctrl('d') if self.line.is_empty() => return Action::Quit,
            Key::Ctrl('d') => self.line.delete(),
            Key::Ctrl('l') => self.output.clear(),
            Key::Up => self.recall_older(),
            Key::Down => self.recall_newer(),
            Key::PageUp => self.scroll = self.scroll.saturating_add(self.page.max(1)),
            Key::PageDown => self.scroll = self.scroll.saturating_sub(self.page.max(1)),
            // Completion comes with the commands (L7); other keys do nothing.
            Key::Tab => self.complete(),
            Key::BackTab | Key::Esc | Key::Ctrl(_) => {}
            // Turned into plain keys by `without_shift` above.
            Key::ShiftLeft
            | Key::ShiftRight
            | Key::ShiftUp
            | Key::ShiftDown
            | Key::ShiftHome
            | Key::ShiftEnd => {}
        }
        Action::Nothing
    }

    fn recall_older(&mut self) {
        let next = match &self.recalled {
            None if self.history.is_empty() => return,
            None => self.history.len() - 1,
            Some((0, _)) => return,
            Some((i, _)) => i - 1,
        };
        if self.recalled.is_none() {
            self.recalled = Some((next, self.line.take()));
        } else if let Some((i, _)) = &mut self.recalled {
            *i = next;
        }
        self.line.set(&self.history[next]);
    }

    fn recall_newer(&mut self) {
        match self.recalled.take() {
            None => {}
            Some((i, draft)) if i + 1 >= self.history.len() => self.line.set(&draft),
            Some((i, draft)) => {
                self.line.set(&self.history[i + 1]);
                self.recalled = Some((i + 1, draft));
            }
        }
    }

    /// Enter: echo the line, remember it, and run it.
    fn submit(&mut self) -> Action {
        self.recalled = None;
        let typed = self.line.take();
        let mut echo = Zeroizing::new(String::with_capacity(self.prompt.len() + typed.len()));
        echo.push_str(&self.prompt);
        echo.push_str(&typed);
        self.print(&echo, ECHO_STYLE);
        if typed.trim().is_empty() {
            return Action::Nothing;
        }
        if self.history.back().map(|h| h.as_str()) != Some(typed.as_str()) {
            if self.history.len() == MAX_HISTORY {
                self.history.pop_front();
            }
            self.history
                .push_back(Zeroizing::new(typed.as_str().to_owned()));
        }
        let words = match parse::words(&typed) {
            Ok(words) => words,
            Err(e) => {
                self.print(&e.to_string(), ERROR_STYLE);
                return Action::Nothing;
            }
        };
        match words.first().map(|w| w.as_str()) {
            None => Action::Nothing,
            Some("help") => {
                self.print(HELP, Style::PLAIN);
                Action::Nothing
            }
            Some("clear") => {
                self.output.clear();
                Action::Nothing
            }
            Some("quit" | "exit") => Action::Quit,
            Some(_) => Action::Command(words),
        }
    }

    /// Output above, the prompt on the bottom row.
    pub fn draw(&mut self, grid: &mut Grid) {
        let (output, prompt_row) = grid.area().split_bottom(1);
        self.page = usize::from(output.height).saturating_sub(1).max(1);
        self.scroll = self.output.draw(grid, output, self.scroll);
        if prompt_row.is_empty() {
            return;
        }
        let y = prompt_row.y;
        let width = prompt_row.width;
        let used = grid.put_str(0, y, &self.prompt, PROMPT_STYLE, width);
        // The text before the cursor, scrolled so the cursor stays visible.
        let room = usize::from(width - used).saturating_sub(1);
        let text = self.line.as_str();
        let (before, after) = text.split_at(self.line.cursor());
        let mut start = 0;
        let mut shown = display_width(before);
        let mut graphemes = before.grapheme_indices(true);
        while shown > room {
            let Some((at, g)) = graphemes.next() else {
                break;
            };
            shown -= display_width(g);
            start = at + g.len();
        }
        let x = used + grid.put_str(used, y, &before[start..], Style::PLAIN, width - used);
        // The cursor: a reversed cell over the next grapheme, or a space.
        let under = after.graphemes(true).next().unwrap_or(" ");
        let cursor = grid.put_str(x, y, under, CURSOR_STYLE, width.saturating_sub(x));
        let rest = &after[under.len().min(after.len())..];
        grid.put_str(
            x + cursor,
            y,
            rest,
            Style::PLAIN,
            width.saturating_sub(x + cursor),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn shell() -> Shell {
        Shell::new(&VaultName::new(OsString::from("diary")).unwrap())
    }

    fn type_text(shell: &mut Shell, text: &str) -> Action {
        let mut last = Action::Nothing;
        for c in text.chars() {
            last = shell.key(if c == '\r' { Key::Enter } else { Key::Char(c) });
        }
        last
    }

    fn screen(shell: &mut Shell, width: u16, height: u16) -> Vec<String> {
        let mut grid = Grid::new(width, height);
        shell.draw(&mut grid);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn prompt_and_echo() {
        let mut s = shell();
        assert_eq!(screen(&mut s, 30, 2), ["", "crystal:diary>"]);
        type_text(&mut s, "help");
        assert_eq!(screen(&mut s, 30, 2)[1], "crystal:diary> help");
        assert!(matches!(s.key(Key::Enter), Action::Nothing));
        let rows = screen(&mut s, 80, 30);
        assert!(rows.iter().any(|r| r == "crystal:diary> help"), "{rows:?}");
        assert!(rows.iter().any(|r| r.starts_with("Commands:")), "{rows:?}");
    }

    #[test]
    fn commands_go_to_the_caller() {
        let mut s = shell();
        match type_text(&mut s, "view \"work/a b\"\r") {
            Action::Command(words) => {
                let words: Vec<&str> = words.iter().map(|w| w.as_str()).collect();
                assert_eq!(words, ["view", "work/a b"]);
            }
            _ => panic!("not a command"),
        }
        assert!(matches!(type_text(&mut s, "quit\r"), Action::Quit));
        assert!(matches!(type_text(&mut s, "exit\r"), Action::Quit));
        assert!(matches!(s.key(Key::Ctrl('d')), Action::Quit));
        assert!(matches!(type_text(&mut s, "x"), Action::Nothing));
        assert!(
            matches!(s.key(Key::Ctrl('d')), Action::Nothing),
            "not on a typed line"
        );
    }

    #[test]
    fn parse_errors_and_clear() {
        let mut s = shell();
        type_text(&mut s, "view \"open\r");
        assert!(
            screen(&mut s, 40, 3)
                .iter()
                .any(|r| r == "a quote is not closed")
        );
        type_text(&mut s, "clear\r");
        assert_eq!(screen(&mut s, 40, 3), ["", "", "crystal:diary>"]);
    }

    #[test]
    fn history_recall() {
        let mut s = shell();
        type_text(&mut s, "first\r");
        type_text(&mut s, "second\r");
        type_text(&mut s, "second\r");
        type_text(&mut s, "draft");
        s.key(Key::Up);
        assert_eq!(s.line.as_str(), "second", "repeats are kept once");
        s.key(Key::Up);
        assert_eq!(s.line.as_str(), "first");
        s.key(Key::Up);
        assert_eq!(s.line.as_str(), "first");
        s.key(Key::Down);
        assert_eq!(s.line.as_str(), "second");
        s.key(Key::Down);
        assert_eq!(s.line.as_str(), "draft", "the draft comes back");
        for i in 0..MAX_HISTORY + 10 {
            type_text(&mut s, &format!("c{i}\r"));
        }
        assert_eq!(s.history.len(), MAX_HISTORY);
    }

    #[test]
    fn paging_and_editing_keys() {
        let mut s = shell();
        for i in 0..20 {
            s.print(&format!("line {i}"), Style::PLAIN);
        }
        assert_eq!(screen(&mut s, 20, 4)[2], "line 19");
        s.key(Key::PageUp);
        assert_eq!(screen(&mut s, 20, 4)[2], "line 17");
        s.key(Key::PageDown);
        assert_eq!(screen(&mut s, 20, 4)[2], "line 19");
        type_text(&mut s, "abc");
        s.key(Key::Left);
        s.key(Key::Ctrl('k'));
        assert_eq!(s.line.as_str(), "ab");
        s.key(Key::Ctrl('c'));
        assert!(s.line.is_empty());
        s.key(Key::Ctrl('l'));
        assert_eq!(screen(&mut s, 20, 2)[0], "");
    }

    // A long line scrolls sideways so the cursor stays on screen.
    #[test]
    fn long_lines_keep_the_cursor_visible() {
        let mut s = shell();
        type_text(&mut s, &"x".repeat(100));
        type_text(&mut s, "END");
        let row = &screen(&mut s, 30, 1)[0];
        assert!(
            row.starts_with("crystal:diary> ") && row.ends_with("END"),
            "{row}"
        );
        assert!(row.chars().count() <= 30);
    }

    #[test]
    fn tab_completes_commands_names_and_folders() {
        let mut s = shell();
        s.set_names(
            [
                "work/projects/Roadmap",
                "work/meeting 2026-11-07",
                "welcome",
            ]
            .iter()
            .map(|n| Zeroizing::new((*n).to_owned()))
            .collect(),
        );
        type_text(&mut s, "vi");
        s.key(Key::Tab);
        assert_eq!(s.line.as_str(), "view ");
        type_text(&mut s, "wo");
        s.key(Key::Tab);
        assert_eq!(s.line.as_str(), "view work/", "a folder, no space");
        type_text(&mut s, "m");
        s.key(Key::Tab);
        assert_eq!(
            s.line.as_str(),
            "view \"work/meeting 2026-11-07\" ",
            "quoted"
        );
        assert!(matches!(s.key(Key::Enter), Action::Command(_)));
        // Several options: the shared part, then a list on the second Tab.
        type_text(&mut s, "view w");
        s.key(Key::Tab);
        assert_eq!(s.line.as_str(), "view w");
        s.key(Key::Tab);
        let rows = screen(&mut s, 80, 4);
        assert!(
            rows.iter()
                .any(|r| r.contains("welcome") && r.contains("work/")),
            "{rows:?}"
        );
        // Nothing matches: nothing changes.
        s.key(Key::Ctrl('u'));
        type_text(&mut s, "zz");
        s.key(Key::Tab);
        assert_eq!(s.line.as_str(), "zz");
    }

    #[test]
    fn hostile_paste_is_inert() {
        let mut s = shell();
        let mut decoder = keys::Decoder::new();
        let mut keys = Vec::new();
        decoder.feed(b"\x1b[200~quit\r\nview \x1b[2J\x1b[201~", &mut keys);
        for key in keys {
            assert!(matches!(s.key(key), Action::Nothing), "a paste never runs");
        }
        assert_eq!(s.line.as_str(), "quit view [2J");
    }

    /// A paste that does not fit the line is cut, and the shell says so;
    /// one that fits says nothing.
    #[test]
    fn a_cut_paste_is_reported() {
        let mut s = shell();
        let before = s.output_len();
        let paste = |text: String, cut| Key::Paste {
            text: Zeroizing::new(text),
            cut,
        };
        s.key(paste("short".into(), false));
        assert_eq!(s.output_len(), before);
        s.key(paste("x".repeat(line::MAX_LINE), false));
        assert_eq!(s.line.as_str().len(), line::MAX_LINE);
        assert_eq!(s.output_len(), before + 1);
        s.key(Key::Ctrl('u'));
        s.key(paste("y".into(), true));
        assert_eq!(s.output_len(), before + 2, "cut by the decoder");
    }
}
