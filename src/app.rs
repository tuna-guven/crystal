//! The unlocked session's loop: the shell, and the full-screen pager and
//! editor it hands the screen to (docs/decisions/0033-shell.md,
//! docs/decisions/0045-simple-editor.md).
//!
//! Draw, wait, handle keys; a resize or resume redraws everything. `view`
//! opens the pager until `q`; `edit` and `new` open the editor until
//! Ctrl+Q. In the editor, Ctrl+S saves through the session and Ctrl+E
//! shows the text rendered, unsaved changes included, until Ctrl+E, `q`
//! or Esc. Ends on `quit`, Ctrl+D, an exit signal or a closed terminal;
//! Crystal's own clipboard is wiped on the way out.

use std::error::Error;

use zeroize::Zeroizing;

use crate::commands::{self, Open};
use crate::editor::view::View;
use crate::editor::{Editor, EditorAction};
use crate::model::NoteText;
use crate::notes::Notes;
use crate::pager::{Pager, PagerAction};
use crate::session::{Change, SaveError, Session};
use crate::shell::keys::Key;
use crate::shell::{self, ERROR_STYLE, Shell};
use crate::term::{Event, Signals, TerminalGuard};
use crate::ui::clipboard::Clipboard;
use crate::ui::grid::Style;
use crate::ui::screen::Screen;

const COPIED: &str = "Copied inside Crystal; it never leaves it.";

/// What has the screen.
enum Mode {
    Shell,
    View(Box<Pager>),
    Edit(Box<Editing>),
}

/// The note being edited.
enum Target {
    Note(usize),
    /// From `new`: not in the vault until the first save.
    New {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
        /// `folder/title`, for the top row.
        name: Zeroizing<String>,
    },
}

/// The editor, its screen and, while Ctrl+E shows it, its preview.
struct Editing {
    editor: Editor,
    view: View,
    target: Target,
    preview: Option<Pager>,
}

/// Runs the session until the user leaves. `demo`: the debug-only
/// `--demo-notes-for-test` notes are in memory, so nothing may be saved.
pub fn run(
    terminal: &mut TerminalGuard,
    signals: &Signals,
    shell: &mut Shell,
    session: &mut Session,
    demo: bool,
) -> Result<(), Box<dyn Error>> {
    let mut clipboard = Clipboard::default();
    let result = run_loop(terminal, signals, shell, session, demo, &mut clipboard);
    clipboard.wipe();
    result
}

fn run_loop(
    terminal: &mut TerminalGuard,
    signals: &Signals,
    shell: &mut Shell,
    session: &mut Session,
    demo: bool,
    clipboard: &mut Clipboard,
) -> Result<(), Box<dyn Error>> {
    let mut screen = Screen::new(terminal.size()?);
    let mut decoder = shell::keys::Decoder::new();
    let mut input = Zeroizing::new([0u8; 256]);
    let mut keys = Vec::new();
    let mut mode = Mode::Shell;
    // `rm` asked "Delete … for good?": the note, until the next line.
    // `keys.drain(..)` drops the keys left when the loop breaks.
    let mut confirm_rm: Option<usize> = None;
    loop {
        screen.frame(terminal.output(), |grid| {
            let notes = session.notes();
            match &mut mode {
                Mode::Shell => shell.draw(grid),
                Mode::View(pager) => pager.draw(grid, notes, None),
                Mode::Edit(e) => match &mut e.preview {
                    Some(pager) => pager.draw(grid, notes, Some(e.editor.text())),
                    None => {
                        let name = match &e.target {
                            Target::Note(note) => notes.name(*note),
                            Target::New { name, .. } => name.as_str(),
                        };
                        e.view.draw(grid, &mut e.editor, name);
                    }
                },
            }
        })?;
        match terminal.wait(signals, &mut input[..], None)? {
            Event::Exit => return Ok(()),
            Event::Redraw | Event::Resumed => screen.redraw_all(terminal.size()?),
            Event::Timeout => {}
            Event::Keys(n) => {
                decoder.feed(&input[..n], &mut keys);
                for key in keys.drain(..) {
                    match &mut mode {
                        Mode::Shell => {
                            let submitted = key == Key::Enter;
                            match shell.key(key) {
                                // Enter on an empty line, help, clear or a
                                // line that does not parse: the shell handled
                                // it, but it still answers rm, and not with y.
                                shell::Action::Nothing => {
                                    if submitted && confirm_rm.take().is_some() {
                                        shell.print("Not deleted.", Style::PLAIN);
                                    }
                                }
                                shell::Action::Quit => return Ok(()),
                                shell::Action::Command(words) => {
                                    // The line after `rm`'s question: only `y`
                                    // deletes. `n` or `no` just answers; any
                                    // other line is a command and still runs.
                                    if let Some(note) = confirm_rm.take() {
                                        let answer = match &words[..] {
                                            [word] => word.as_str(),
                                            _ => "",
                                        };
                                        if answer == "y" {
                                            let change = Change::Remove { note };
                                            save_from_shell(
                                                session, change, shell, demo, "Deleted.",
                                            );
                                            continue;
                                        }
                                        shell.print("Not deleted.", Style::PLAIN);
                                        if matches!(answer, "n" | "no") {
                                            continue;
                                        }
                                    }
                                    match commands::run(session.notes(), &words, shell) {
                                        Some(Open::View(note)) => {
                                            mode = Mode::View(Box::new(Pager::open(note)));
                                        }
                                        Some(Open::Edit(note)) => {
                                            mode = edit(session.notes(), Target::Note(note), shell);
                                        }
                                        Some(Open::New { folder, title }) => {
                                            let name = full_name(&folder, &title);
                                            let target = Target::New {
                                                folder,
                                                title,
                                                name,
                                            };
                                            mode = edit(session.notes(), target, shell);
                                        }
                                        Some(Open::Move {
                                            note,
                                            folder,
                                            title,
                                        }) => {
                                            let change = Change::Rename {
                                                note,
                                                folder: text(&folder),
                                                title: text(&title),
                                            };
                                            save_from_shell(session, change, shell, demo, "Moved.");
                                        }
                                        Some(Open::Remove(note)) => {
                                            let question = [
                                                "Delete ",
                                                session.notes().name(note),
                                                " for good? There is no trash: only a backup brings \
                                             it back. Type y and Enter to delete.",
                                            ];
                                            shell.print(&joined(&question), Style::PLAIN);
                                            confirm_rm = Some(note);
                                            // The answer must come from a later
                                            // read, typed after the question was
                                            // seen: drop the rest of this batch
                                            // (a pasted "rm NAME⏎y⏎").
                                            break;
                                        }
                                        None => {}
                                    }
                                }
                            }
                        }
                        Mode::View(pager) => match pager.key(key, session.notes()) {
                            PagerAction::Stay => {}
                            PagerAction::Close => mode = Mode::Shell,
                            PagerAction::Edit(note) => {
                                mode = edit(session.notes(), Target::Note(note), shell);
                            }
                            PagerAction::Copy(note) => {
                                clipboard.set(session.notes().note(note).body.as_str());
                                pager.tell(COPIED);
                            }
                        },
                        Mode::Edit(e) => {
                            if let Some(pager) = &mut e.preview {
                                match pager.key(key, session.notes()) {
                                    PagerAction::Stay => {}
                                    // Ctrl+E, q or Esc: back to the editor,
                                    // whichever note the preview has reached.
                                    PagerAction::Close | PagerAction::Edit(_) => e.preview = None,
                                    PagerAction::Copy(note) => {
                                        let previewed =
                                            matches!(e.target, Target::Note(n) if n == note);
                                        clipboard.set(if previewed {
                                            e.editor.text()
                                        } else {
                                            session.notes().note(note).body.as_str()
                                        });
                                        pager.tell(COPIED);
                                    }
                                }
                                continue;
                            }
                            match e.editor.key(key, clipboard) {
                                EditorAction::Stay => {}
                                EditorAction::Close => mode = Mode::Shell,
                                EditorAction::View => match e.target {
                                    Target::Note(note) => e.preview = Some(Pager::preview(note)),
                                    Target::New { .. } => e
                                        .editor
                                        .tell("A new note is shown rendered once Ctrl+S saved it."),
                                },
                                EditorAction::Save => save(session, e, shell, demo),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Opens the editor on `target`: a saved note's text, or an empty one.
fn edit(notes: &Notes, target: Target, shell: &mut Shell) -> Mode {
    let body = match &target {
        Target::Note(note) => notes.note(*note).body.as_str(),
        Target::New { .. } => "",
    };
    match Editor::new(body) {
        Ok(editor) => Mode::Edit(Box::new(Editing {
            editor,
            view: View::default(),
            target,
            preview: None,
        })),
        // The vault's own body limit makes this unreachable.
        Err(_) => {
            shell.print("That note is too large for the editor.", ERROR_STYLE);
            Mode::Shell
        }
    }
}

/// Ctrl+S: saves the editor's text through the session. The editor says
/// how it went; the full error, numbers included but never note text, goes
/// to the shell for after Ctrl+Q.
fn save(session: &mut Session, e: &mut Editing, shell: &mut Shell, demo: bool) {
    if demo {
        e.editor
            .tell("Not saved: demo notes (--demo-notes-for-test) are never saved.");
        return;
    }
    let body = text(e.editor.text());
    let change = match &e.target {
        Target::Note(note) => Change::Body {
            note: *note,
            text: body,
        },
        Target::New { folder, title, .. } => Change::New {
            folder: text(folder),
            title: text(title),
            body,
        },
    };
    let result = session.save(change);
    let saved = result.as_ref().map_or_else(SaveError::is_saved, |()| true);
    if saved {
        e.editor.saved();
        if matches!(e.target, Target::New { .. }) {
            // A new note is pushed last.
            e.target = Target::Note(session.notes().len() - 1);
        }
        shell.set_names(names(session.notes()));
    }
    match result {
        Ok(()) => e.editor.tell("Saved."),
        Err(err) => {
            shell.print(&err.to_string(), ERROR_STYLE);
            e.editor.tell(match err {
                SaveError::Changed => {
                    "Not saved: vault.age changed on disk (synced?). Your edits are only \
                     here and are lost when Crystal closes."
                }
                _ if saved => "Saved, with a warning: see the shell after Ctrl+Q.",
                _ => "Not saved: see the shell after Ctrl+Q for why.",
            });
        }
    }
}

/// `mv` and `rm`: saves `change` and says how it went in the shell. The
/// shell's completion learns the names again either way.
fn save_from_shell(
    session: &mut Session,
    change: Change,
    shell: &mut Shell,
    demo: bool,
    done: &'static str,
) {
    if demo {
        shell.print(
            "Not saved: demo notes (--demo-notes-for-test) are never saved.",
            ERROR_STYLE,
        );
        return;
    }
    match session.save(change) {
        Ok(()) => shell.print(done, Style::PLAIN),
        Err(e) => shell.print(&e.to_string(), ERROR_STYLE),
    }
    shell.set_names(names(session.notes()));
}

/// `parts` as one line, built at its final size.
fn joined(parts: &[&str]) -> Zeroizing<String> {
    let mut line = Zeroizing::new(String::with_capacity(parts.iter().map(|p| p.len()).sum()));
    for part in parts {
        line.push_str(part);
    }
    line
}

/// Note text, copied straight into zeroizing memory.
fn text(s: &str) -> NoteText {
    NoteText::from_zeroizing(Zeroizing::new(s.to_owned()))
}

/// `folder/title`, or `title` at the top level, built at its final size.
fn full_name(folder: &str, title: &str) -> Zeroizing<String> {
    let mut name = Zeroizing::new(String::with_capacity(folder.len() + 1 + title.len()));
    if !folder.is_empty() {
        name.push_str(folder);
        name.push('/');
    }
    name.push_str(title);
    name
}

/// Every note's name, for the shell's completion.
pub fn names(notes: &Notes) -> Vec<Zeroizing<String>> {
    (0..notes.len())
        .map(|i| Zeroizing::new(notes.name(i).to_owned()))
        .collect()
}
