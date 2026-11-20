//! The unlocked session's loop: the shell, and the full-screen pager and
//! editor it hands the screen to (decision 0033,
//! decision 0045).
//!
//! Draw, wait, handle keys; a resize or resume redraws everything. `view`
//! opens the pager until `q`; `edit` and `new` open the editor until
//! Ctrl+Q. In the editor, Ctrl+S saves through the session; Ctrl+Q and
//! Ctrl+E save first too, then close or show the text rendered (from the
//! cursor's block) until Ctrl+E, `q` or Esc. Ends on `quit`, Ctrl+D, an exit signal or a closed terminal,
//! and locks on `lock` or after `AUTO_LOCK` without a key or paste
//! (decision 0048); Crystal's own clipboard is
//! wiped on the way out.

use std::error::Error;
use std::time::Duration;

use zeroize::Zeroizing;

use crate::commands::{self, Open};
use crate::editor::live::fragment_at;
use crate::editor::suggest::{ID_FULL, ID_REFUSED};
use crate::editor::view::{Click, LIVE_MAX, View};
use crate::editor::{Editor, EditorAction, UNSAVED};
use crate::md::blocks::{add_id, block_line, blocks};
use crate::md::render::{Kind, kind_of, shown};
use crate::model::{NoteText, lines};
use crate::notes::Notes;
use crate::pager::{Pager, PagerAction, WEB_LINK};
use crate::session::{Change, SaveError, Session};
use crate::shell::keys::{Key, MouseEvent};
use crate::shell::{self, ERROR_STYLE, Shell};
use crate::term::{Event, Idle, Signals, TerminalGuard, WAKE_CHECK};
use crate::ui::clipboard::Clipboard;
use crate::ui::grid::Style;
use crate::ui::screen::{Mouse, Screen};
use crate::vault::codec::MAX_BODY_BYTES;

const COPIED: &str = "Copied inside Crystal; it never leaves it.";
/// Ctrl+E in the preview, on a note reached by a link, while the editor
/// holds unsaved changes.
const SAVE_FIRST: &str =
    "Unsaved changes here: Ctrl+S saves them, then Ctrl+E edits the other note.";
/// A click on a link to another note in the live editor, while it holds
/// unsaved changes (step LF3).
const SAVE_FIRST_LINK: &str =
    "Unsaved changes here: Ctrl+S saves them, then a click follows the link.";

/// Auto-lock after this long without a key or paste, fixed (decision 0048).
pub const AUTO_LOCK: Duration = Duration::from_secs(5 * 60);

/// What has the screen.
enum Mode {
    Shell,
    View(Box<Pager>),
    Edit(Box<Editing>),
}

/// The note being edited.
enum Target {
    Note(usize),
    /// Created by `edit` (decision 0038): not in the vault until the
    /// first save. `title` is the stored one, `.txt` for text.
    New {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
        /// `folder/title`, for the top row.
        name: Zeroizing<String>,
    },
}

impl Target {
    /// Whether this is `note`, saved (a new note is no note yet).
    fn is(&self, note: usize) -> bool {
        matches!(self, Self::Note(n) if *n == note)
    }
}

/// A question the shell asked; the next line answers it.
enum Question {
    /// `rm`: "Delete … for good?" Only `y` deletes.
    Remove(usize),
    /// `edit` of a missing note: "Create it?" Only `y` goes on.
    Create {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
    },
    /// Then its type: `m` or a blank line for Markdown, `t` for text.
    Kind {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
    },
}

/// The line typed after a question.
#[derive(Clone, Copy)]
enum Answer<'a> {
    /// Enter on a blank line.
    Blank,
    /// A line of one word.
    Word(&'a str),
    /// Any other line, `help` and `clear` too.
    Other,
}

/// What an answer does.
enum Reply {
    Delete(usize),
    AskKind {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
    },
    Create {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
        kind: Kind,
    },
    /// Nothing is done, and this is said.
    Declined(&'static str),
}

const CREATE_QUESTION: &str = " does not exist. Create it? Type y and Enter.";
const KIND_QUESTION: &str =
    "Markdown or text? Type m or t and Enter (Enter alone: Markdown). It cannot change later.";

/// The answer to `question`. A blank line keeps its meaning only for the
/// type, where it is the default; elsewhere it says no.
fn reply(question: Question, answer: Answer<'_>) -> Reply {
    match (question, answer) {
        (Question::Remove(note), Answer::Word("y")) => Reply::Delete(note),
        (Question::Remove(_), _) => Reply::Declined("Not deleted."),
        (Question::Create { folder, title }, Answer::Word("y")) => Reply::AskKind { folder, title },
        (Question::Kind { folder, title }, Answer::Blank | Answer::Word("m")) => Reply::Create {
            folder,
            title,
            kind: Kind::Markdown,
        },
        (Question::Kind { folder, title }, Answer::Word("t")) => Reply::Create {
            folder,
            title,
            kind: Kind::Plain,
        },
        (Question::Create { .. } | Question::Kind { .. }, _) => Reply::Declined("Not created."),
    }
}

/// Carries out `reply`. True when it asked a new question: the answer must
/// come from a later read, typed after the question was seen, so the
/// caller drops the rest of the keys (a pasted "edit NAME⏎y⏎").
fn act(
    reply: Reply,
    mode: &mut Mode,
    question: &mut Option<Question>,
    session: &mut Session,
    shell: &mut Shell,
    demo: bool,
) -> bool {
    match reply {
        Reply::Delete(note) => {
            save_from_shell(session, Change::Remove { note }, shell, demo, "Deleted.");
        }
        Reply::AskKind { folder, title } => {
            shell.print(KIND_QUESTION, Style::PLAIN);
            *question = Some(Question::Kind { folder, title });
            return true;
        }
        Reply::Create {
            folder,
            title,
            kind,
        } => {
            if let Some(title) = commands::stored_title(&title, kind, shell) {
                let name = full_name(&folder, &title);
                let target = Target::New {
                    folder,
                    title,
                    name,
                };
                *mode = edit(session.notes(), target, false, shell);
            }
        }
        Reply::Declined(said) => shell.print(said, Style::PLAIN),
    }
    false
}

/// The editor, its screen and, while Ctrl+E shows it, its preview.
struct Editing {
    editor: Editor,
    view: View,
    target: Target,
    preview: Option<Pager>,
}

/// Why a session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `quit`, an exit signal or a closed terminal: Crystal exits.
    Quit,
    /// The caller wipes the session and asks for the passphrase again
    /// (decision 0048).
    Lock(LockReason),
}

/// Why Crystal locked, for the lock screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReason {
    /// The `lock` command.
    Command,
    /// No key or paste for `AUTO_LOCK`, and what became of unsaved edits.
    Idle(Edits),
    /// SIGTSTP: the caller stops the process after the lock.
    Suspended(Edits),
}

/// Unsaved edits when auto-lock fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edits {
    /// The editor was closed, or had nothing unsaved.
    None,
    /// Saved first, as Ctrl+S would.
    Saved,
    /// The save was refused or failed (or demo notes): they are gone.
    Lost,
}

/// Runs the session until the user leaves or locks. Crystal's clipboard
/// is wiped either way, and the pager and editor, which live in this
/// call, are dropped (wiped) when it returns. `demo`: the debug-only
/// `--demo-notes-for-test` notes are in memory, so nothing may be saved.
pub fn run(
    terminal: &mut TerminalGuard,
    signals: &Signals,
    shell: &mut Shell,
    session: &mut Session,
    demo: bool,
    idle: Duration,
) -> Result<Outcome, Box<dyn Error>> {
    let mut clipboard = Clipboard::default();
    let result = run_loop(
        terminal,
        signals,
        shell,
        session,
        demo,
        idle,
        &mut clipboard,
    );
    clipboard.wipe();
    result
}

fn run_loop(
    terminal: &mut TerminalGuard,
    signals: &Signals,
    shell: &mut Shell,
    session: &mut Session,
    demo: bool,
    idle: Duration,
    clipboard: &mut Clipboard,
) -> Result<Outcome, Box<dyn Error>> {
    let mut screen = Screen::new(terminal.size()?);
    let mut decoder = shell::keys::Decoder::new();
    let mut input = Zeroizing::new([0u8; 256]);
    let mut keys = Vec::new();
    let mut mode = Mode::Shell;
    // A question `rm` or `edit` asked, until the next line.
    // `keys.drain(..)` drops the keys left when the loop breaks.
    let mut question: Option<Question> = None;
    // The last key or paste; a resize does not count. Sleep counts, and a
    // changed system time cannot delay auto-lock (decision 0050).
    let mut last_input = Idle::start();
    // A SIGTSTP sent during the passphrase prompt was not acted on there.
    signals.forget_suspend();
    loop {
        // Mouse reports only while the editor (decision 0073) or the
        // viewer, the preview too (decision 0076), shows; never in the
        // shell.
        let mouse = match &mode {
            // Live mode shows heading markers under the mouse (step LF2).
            Mode::Edit(e) if e.preview.is_none() && !e.view.is_live() => Mouse::Buttons,
            Mode::Edit(_) | Mode::View(_) => Mouse::Motion,
            Mode::Shell => Mouse::Off,
        };
        screen.frame(terminal.output(), mouse, |grid| {
            let notes = session.notes();
            match &mut mode {
                Mode::Shell => shell.draw(grid),
                Mode::View(pager) => pager.draw(grid, notes, None),
                Mode::Edit(e) => match &mut e.preview {
                    Some(pager) => pager.draw(grid, notes, Some(e.editor.text())),
                    None => {
                        let (name, _) = shown(match &e.target {
                            Target::Note(note) => notes.name(*note),
                            Target::New { name, .. } => name.as_str(),
                        });
                        e.view.draw(grid, &mut e.editor, name);
                        // The link suggestions over it, named from the
                        // session's notes (borrowed, never copied).
                        e.view.draw_suggestions(grid, &e.editor, notes);
                    }
                },
            }
        })?;
        let left = idle.saturating_sub(last_input.elapsed()).min(WAKE_CHECK);
        match terminal.wait(signals, &mut input[..], Some(left))? {
            Event::Exit => return Ok(Outcome::Quit),
            Event::Redraw => screen.redraw_all(terminal.size()?),
            // SIGTSTP: lock first, so the stopped process holds no notes;
            // the caller stops it (decision 0048).
            Event::Suspend => {
                let edits = save_before_lock(&mut mode, session, shell, demo);
                return Ok(Outcome::Lock(LockReason::Suspended(edits)));
            }
            Event::Timeout if idle_passed(last_input.elapsed(), idle) => {
                let edits = save_before_lock(&mut mode, session, shell, demo);
                return Ok(Outcome::Lock(LockReason::Idle(edits)));
            }
            Event::Timeout => {}
            Event::Keys(n) => {
                // A read that filled `input` has more bytes waiting.
                decoder.feed(&input[..n], n < input.len(), &mut keys);
                if is_activity(&keys) {
                    last_input = Idle::start();
                } else if idle_passed(last_input.elapsed(), idle) {
                    // Only motion (or nothing) past the limit: lock now,
                    // so a stream of it cannot hold the lock off by never
                    // letting a wait end empty (decision 0076).
                    keys.clear();
                    let edits = save_before_lock(&mut mode, session, shell, demo);
                    return Ok(Outcome::Lock(LockReason::Idle(edits)));
                }
                for key in keys.drain(..) {
                    match &mut mode {
                        Mode::Shell => {
                            let submitted = key == Key::Enter;
                            match shell.key(key) {
                                // Enter on a blank line, help, clear or a
                                // line that does not parse: the shell handled
                                // it, but it still answers a question.
                                action @ (shell::Action::Nothing | shell::Action::Empty) => {
                                    if submitted && let Some(asked) = question.take() {
                                        let answer = match action {
                                            shell::Action::Empty => Answer::Blank,
                                            _ => Answer::Other,
                                        };
                                        let reply = reply(asked, answer);
                                        let (m, q) = (&mut mode, &mut question);
                                        if act(reply, m, q, session, shell, demo) {
                                            break;
                                        }
                                    }
                                }
                                shell::Action::Quit => return Ok(Outcome::Quit),
                                shell::Action::Lock => {
                                    return Ok(Outcome::Lock(LockReason::Command));
                                }
                                shell::Action::Command(words) => {
                                    // The line after a question answers it. A
                                    // declined line other than `n` or `no` is a
                                    // command and still runs.
                                    if let Some(asked) = question.take() {
                                        let answer = match &words[..] {
                                            [word] => Answer::Word(word.as_str()),
                                            _ => Answer::Other,
                                        };
                                        let reply = reply(asked, answer);
                                        let declined = matches!(reply, Reply::Declined(_));
                                        let (m, q) = (&mut mode, &mut question);
                                        if act(reply, m, q, session, shell, demo) {
                                            break;
                                        }
                                        if !declined || matches!(answer, Answer::Word("n" | "no")) {
                                            continue;
                                        }
                                    }
                                    match commands::run(session.notes(), &words, shell) {
                                        Some(Open::View(note)) => {
                                            mode = Mode::View(Box::new(Pager::open(note)));
                                        }
                                        Some(Open::Edit { note, live }) => {
                                            let target = Target::Note(note);
                                            mode = edit(session.notes(), target, live, shell);
                                        }
                                        Some(Open::Create { folder, title }) => {
                                            let name = full_name(&folder, &title);
                                            shell.print(
                                                &joined(&[&name, CREATE_QUESTION]),
                                                Style::PLAIN,
                                            );
                                            question = Some(Question::Create { folder, title });
                                            // As for rm: the answer comes later.
                                            break;
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
                                            let asked = [
                                                "Delete ",
                                                session.notes().name(note),
                                                " for good? There is no trash: only a backup brings \
                                             it back. Type y and Enter to delete.",
                                            ];
                                            shell.print(&joined(&asked), Style::PLAIN);
                                            question = Some(Question::Remove(note));
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
                                mode = edit(session.notes(), Target::Note(note), false, shell);
                            }
                            PagerAction::Copy(note) => {
                                clipboard.set(session.notes().note(note).body.as_str());
                                pager.tell(COPIED);
                            }
                            PagerAction::CopyLine(note, line) => {
                                let text = session.notes().note(note).body.as_str();
                                clipboard.set(lines::numbered(text, line));
                                pager.tell(COPIED);
                            }
                        },
                        Mode::Edit(e) => {
                            if let Some(pager) = &mut e.preview {
                                let action = pager.key(key, session.notes());
                                match action {
                                    PagerAction::Stay => {}
                                    // Ctrl+E on a note a link led to: that
                                    // note in the editor, live if this one
                                    // is; with unsaved changes, back here
                                    // first, so nothing is lost.
                                    PagerAction::Edit(note) if !e.target.is(note) => {
                                        if e.editor.is_dirty() {
                                            e.preview = None;
                                            e.editor.tell(SAVE_FIRST);
                                        } else {
                                            let live = e.view.is_live();
                                            let target = Target::Note(note);
                                            mode = edit(session.notes(), target, live, shell);
                                        }
                                    }
                                    // Ctrl+E on the note being edited, q or
                                    // Esc: back to the editor.
                                    PagerAction::Close | PagerAction::Edit(_) => e.preview = None,
                                    // The text previewed is the editor's,
                                    // unsaved; a note reached by a link is
                                    // its saved one.
                                    PagerAction::Copy(note) | PagerAction::CopyLine(note, _) => {
                                        let text = if e.target.is(note) {
                                            e.editor.text()
                                        } else {
                                            session.notes().note(note).body.as_str()
                                        };
                                        clipboard.set(match action {
                                            PagerAction::CopyLine(_, line) => {
                                                lines::numbered(text, line)
                                            }
                                            _ => text,
                                        });
                                        pager.tell(COPIED);
                                    }
                                }
                                continue;
                            }
                            // The cell, as the last frame drew it, to the
                            // text; a press must land on the text, a drag
                            // stops at its edge (decision 0073). The list
                            // of link suggestions, drawn over the text,
                            // comes first.
                            // Live mode first: a heading's marker, or a
                            // link to follow (steps LF2, LF3).
                            if let Key::Mouse { event, column, row } = key {
                                match e.view.mouse(e.editor.text(), event, column, row) {
                                    Click::Taken => continue,
                                    Click::Link { target, web } => {
                                        if let Some(next) =
                                            follow(session.notes(), e, target, web, shell)
                                        {
                                            mode = next;
                                        }
                                        continue;
                                    }
                                    Click::Editor => {}
                                }
                            }
                            let action = if let Key::Mouse { event, column, row } = key {
                                let clamp = event != MouseEvent::Press;
                                let at = e.view.position_at(e.editor.text(), column, row, clamp);
                                let entry = e.view.suggestion_at(column, row);
                                let now = std::time::Instant::now();
                                e.editor.mouse_in(event, at, entry, now, session.notes())
                            } else {
                                e.editor.key_in(key, clipboard, session.notes())
                            };
                            match action {
                                EditorAction::Stay => {}
                                EditorAction::Close => mode = Mode::Shell,
                                // Leaving saves first; a failed save stays
                                // (Ctrl+Q right after closes).
                                EditorAction::SaveAndClose => {
                                    save(session, e, shell, demo);
                                    if e.editor.is_dirty() {
                                        e.editor.tell(UNSAVED);
                                    } else {
                                        mode = Mode::Shell;
                                    }
                                }
                                EditorAction::View => {
                                    // Demo notes are never saved: shown
                                    // unsaved.
                                    if e.editor.is_dirty() && !demo {
                                        save(session, e, shell, demo);
                                    }
                                    let line = block_line(e.editor.text(), e.editor.cursor());
                                    match e.target {
                                        _ if e.editor.is_dirty() && !demo => {}
                                        Target::Note(note) => {
                                            e.preview = Some(Pager::preview(note, line));
                                        }
                                        Target::New { .. } => e.editor.tell(
                                            "A new note is shown rendered once Ctrl+S saved it.",
                                        ),
                                    }
                                }
                                EditorAction::Save => save(session, e, shell, demo),
                                EditorAction::LinkBlock => link_block(session, e, shell, demo),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Whether `keys` hold off auto-lock: any key, paste, click or wheel, but
/// not the mouse moving with no button held, or a mouse left over the
/// window would keep the vault unlocked (decision 0076). Bytes that
/// decode to nothing (a dropped report, the start of a sequence) do not
/// count either.
fn is_activity(keys: &[Key]) -> bool {
    keys.iter().any(|key| {
        !matches!(
            key,
            Key::Mouse {
                event: MouseEvent::Move,
                ..
            }
        )
    })
}

/// Whether `idle_for` without activity reaches the auto-lock limit.
fn idle_passed(idle_for: Duration, idle: Duration) -> bool {
    idle_for >= idle
}

/// Auto-lock and suspend: an editor with unsaved changes (its preview too) is saved
/// first, exactly as Ctrl+S would; if it is still unsaved afterwards, the
/// save was refused or failed and the edits are lost with the lock.
fn save_before_lock(
    mode: &mut Mode,
    session: &mut Session,
    shell: &mut Shell,
    demo: bool,
) -> Edits {
    let Mode::Edit(e) = mode else {
        return Edits::None;
    };
    if !e.editor.is_dirty() {
        return Edits::None;
    }
    save(session, e, shell, demo);
    if e.editor.is_dirty() {
        Edits::Lost
    } else {
        Edits::Saved
    }
}

/// Follows a link clicked in the live editor (step LF3), as the viewer
/// does: a web link is the terminal's (Ctrl+click); an internal one moves
/// the cursor within this note, or opens its note live at its heading or
/// block once this note's changes are saved; a broken one says why.
/// Returns the mode to switch to, if another note opens.
fn follow(
    notes: &Notes,
    e: &mut Editing,
    target: std::ops::Range<usize>,
    web: bool,
    shell: &mut Shell,
) -> Option<Mode> {
    if web {
        e.editor.tell(WEB_LINK);
        return None;
    }
    // The text may have changed since the frame the click is on.
    let target = Zeroizing::new(e.editor.text().get(target)?.to_owned());
    let Target::Note(from) = e.target else {
        e.editor
            .tell("A new note's links work once Ctrl+S saved it.");
        return None;
    };
    match notes.resolve(from, &target) {
        Err(why) => {
            e.editor.tell(why.describe());
            None
        }
        Ok((note, fragment)) if note == from => {
            if let Some(at) = fragment.and_then(|f| fragment_at(e.editor.text(), &f)) {
                e.editor.jump_to(at);
            }
            None
        }
        Ok(_) if e.editor.is_dirty() => {
            e.editor.tell(SAVE_FIRST_LINK);
            None
        }
        Ok((note, fragment)) => {
            let mut mode = edit(notes, Target::Note(note), true, shell);
            if let (Mode::Edit(next), Some(fragment)) = (&mut mode, fragment)
                && let Some(at) = fragment_at(notes.note(note).body.as_str(), &fragment)
            {
                next.editor.jump_to(at);
            }
            Some(mode)
        }
    }
}

/// Opens the editor on `target`: a saved note's text, or an empty one;
/// `live` shows a Markdown note's markup styled (steps LE1 to LE7), a text
/// note opens as usual.
fn edit(notes: &Notes, target: Target, live: bool, shell: &mut Shell) -> Mode {
    let (body, own, title) = match &target {
        Target::Note(note) => {
            let note_ref = notes.note(*note);
            (note_ref.body.as_str(), Some(*note), note_ref.title.as_str())
        }
        Target::New { title, .. } => ("", None, title.as_str()),
    };
    let markdown = kind_of(title) == Kind::Markdown;
    match Editor::new(body) {
        Ok(mut editor) => Mode::Edit(Box::new(Editing {
            // For link suggestions into its own headings (decision 0075).
            editor: {
                editor.set_note(own, markdown);
                if live && markdown && body.len() > LIVE_MAX {
                    editor.tell("Too large for live mode: shown as written.");
                }
                editor
            },
            view: if live && markdown {
                View::live()
            } else {
                View::default()
            },
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
            // A new note is pushed last; its type does not change.
            let note = session.notes().len() - 1;
            e.target = Target::Note(note);
            let markdown = kind_of(session.notes().note(note).title.as_str()) == Kind::Markdown;
            e.editor.set_note(Some(note), markdown);
        }
        shell.set_names(names(session.notes()));
    }
    match result {
        Ok(()) => e.editor.tell("Saved."),
        Err(err) => failed(&err, saved, e, shell),
    }
}

/// A save that did not go as planned: the full error, never note text,
/// to the shell for after Ctrl+Q, and a short notice in the editor.
fn failed(err: &SaveError, saved: bool, e: &mut Editing, shell: &mut Shell) {
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

/// A block of another note without an id was chosen in the link
/// suggestions (decision 0075): the new id goes into that note first,
/// through the same save as Ctrl+S, and only once it is saved does the
/// editor write the link. The block must still be in the saved text,
/// still without an id, and `add_id` must accept the result. The
/// editor's own text is not saved.
fn link_block(session: &mut Session, e: &mut Editing, shell: &mut Shell, demo: bool) {
    let Some(request) = e.editor.take_block_request() else {
        return;
    };
    if demo {
        e.editor
            .tell("Not linked: demo notes (--demo-notes-for-test) are never saved.");
        return;
    }
    let notes = session.notes();
    // The editor never asks for its own note; refused all the same, as
    // saving it here would drop the editor's unsaved text.
    let own = matches!(e.target, Target::Note(note) if note == request.note);
    let markdown = |note: usize| kind_of(notes.note(note).title.as_str()) == Kind::Markdown;
    let with_id = (!own && request.note < notes.len() && markdown(request.note))
        .then(|| notes.note(request.note).body.as_str())
        .and_then(|body| {
            let block = blocks(body)
                .into_iter()
                .find(|block| block.text == request.block)?;
            add_id(body, &block, request.id())
        });
    let Some(with_id) = with_id else {
        e.editor.tell(ID_REFUSED);
        return;
    };
    if with_id.len() > MAX_BODY_BYTES {
        e.editor.tell(ID_FULL);
        return;
    }
    let result = session.save(Change::Body {
        note: request.note,
        text: NoteText::from_zeroizing(with_id),
    });
    let saved = result.as_ref().map_or_else(SaveError::is_saved, |()| true);
    let linked = saved && e.editor.link_block(&request);
    match result {
        Ok(()) if linked => e.editor.tell("Linked; the new ^id is saved in that note."),
        Ok(()) => e
            .editor
            .tell("The new ^id is saved in that note; the link changed, so it was not written."),
        Err(err) => failed(&err, saved, e, shell),
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

/// Every note's shown name (a text note's without its `.txt`, decision
/// 0038), for the shell's completion.
pub fn names(notes: &Notes) -> Vec<Zeroizing<String>> {
    (0..notes.len())
        .map(|i| Zeroizing::new(shown(notes.name(i)).0.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the mouse moving with no button held, or nothing decoded, is
    /// not activity (decision 0076).
    #[test]
    fn moving_the_mouse_is_not_activity() {
        let at = |event| Key::Mouse {
            event,
            column: 3,
            row: 4,
        };
        assert!(!is_activity(&[]));
        assert!(!is_activity(&[at(MouseEvent::Move), at(MouseEvent::Move)]));
        assert!(is_activity(&[Key::Char('a')]));
        assert!(is_activity(&[at(MouseEvent::Move), Key::Down]));
        // Past the limit, input without activity locks at once.
        let limit = Duration::from_secs(3);
        assert!(idle_passed(limit, limit));
        assert!(!idle_passed(limit - Duration::from_millis(1), limit));
        for event in [
            MouseEvent::Press,
            MouseEvent::Drag,
            MouseEvent::Release,
            MouseEvent::WheelUp,
            MouseEvent::WheelDown,
        ] {
            assert!(is_activity(&[at(MouseEvent::Move), at(event)]), "{event:?}");
        }
    }

    /// Completion offers the shown names (decision 0038).
    #[test]
    fn completion_names_are_shown_names() {
        use crate::notes::tests::{note, notes};
        let n = notes(vec![
            note(1, "work", "todo.txt", ""),
            note(2, "", "plan", ""),
        ]);
        let names: Vec<String> = names(&n).iter().map(|s| s.to_string()).collect();
        assert_eq!(names, ["work/todo", "plan"]);
    }

    fn create() -> Question {
        Question::Create {
            folder: Zeroizing::new("work".to_owned()),
            title: Zeroizing::new("plan".to_owned()),
        }
    }

    fn kind() -> Question {
        Question::Kind {
            folder: Zeroizing::new("work".to_owned()),
            title: Zeroizing::new("plan".to_owned()),
        }
    }

    /// Only `y` deletes or goes on to the type; at the type, a blank line
    /// or `m` is Markdown and `t` text; everything else declines
    /// (decisions 0038 and 0045).
    #[test]
    fn answers_to_questions() {
        use Answer::{Blank, Other, Word};
        assert!(matches!(
            reply(Question::Remove(3), Word("y")),
            Reply::Delete(3)
        ));
        for no in [Blank, Other, Word("n"), Word("Y"), Word("yes")] {
            assert!(matches!(
                reply(Question::Remove(3), no),
                Reply::Declined("Not deleted.")
            ));
        }
        match reply(create(), Word("y")) {
            Reply::AskKind { folder, title } => {
                assert_eq!((folder.as_str(), title.as_str()), ("work", "plan"));
            }
            _ => panic!("y did not ask for the type"),
        }
        for no in [Blank, Other, Word("n"), Word("m")] {
            assert!(matches!(
                reply(create(), no),
                Reply::Declined("Not created.")
            ));
        }
        for (answer, want) in [
            (Blank, Kind::Markdown),
            (Word("m"), Kind::Markdown),
            (Word("t"), Kind::Plain),
        ] {
            match reply(kind(), answer) {
                Reply::Create { kind, .. } => assert_eq!(kind, want),
                _ => panic!("no note created"),
            }
        }
        for no in [Other, Word("n"), Word("y"), Word("text")] {
            assert!(matches!(reply(kind(), no), Reply::Declined("Not created.")));
        }
    }
}
