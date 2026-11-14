//! Crystal: an encrypted Markdown vault in a terminal shell.
//!
//! See the book in docs/ (docs/SUMMARY.md) for the architecture and threat
//! model.

// The real program has no unsafe code, and nothing can switch this off.
#![cfg_attr(not(test), forbid(unsafe_code))]
// Test builds only: still an error by default, but a #[cfg(test)] function
// that checks memory (zeroization) may allow it, with a SAFETY comment.
#![cfg_attr(test, deny(unsafe_code))]

mod age_v1;
mod app;
mod cli;
mod commands;
mod crypto;
mod editor;
mod harden;
mod init;
mod md;
mod model;
mod notes;
mod pager;
mod recover;
mod recovery;
mod rm;
mod session;
mod shell;
mod sys;
mod term;
mod ui;
mod unlock;
mod vault;

use std::error::Error;
use std::process::ExitCode;

fn main() -> ExitCode {
    harden::install_panic_hook();
    match startup() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            print_error(&*e);
            ExitCode::FAILURE
        }
    }
}

/// One of the two places allowed to write to stderr (clippy.toml); the
/// other is the panic hook. Error types hold only fixed text and errnos.
#[expect(clippy::disallowed_macros, reason = "the single error line")]
fn print_error(e: &dyn Error) {
    eprintln!("crystal: {e}");
}

/// --help output: fixed text, so stdout is fine even when redirected.
#[expect(clippy::disallowed_macros, reason = "fixed usage text only")]
fn print_usage() {
    println!("{}", cli::USAGE);
}

/// The result line of `check-code` and `rm`: fixed text, never a name.
#[expect(clippy::disallowed_macros, reason = "fixed result text only")]
fn print_done(message: &dyn std::fmt::Display) {
    println!("{message}");
}

/// docs/design/startup.md, steps 2 onward. Each step fails closed.
/// Errors carry only step names and numbers, never note text.
fn startup() -> Result<(), Box<dyn Error>> {
    harden::lock_down_process()?;
    // Before Crystal opens any fd of its own.
    harden::refuse_inherited_fds()?;
    // Step 3: the environment is read only through harden::env_var (below).
    harden::lock_memory()?;
    let tty = term::Tty::open()?;
    let args = match cli::parse(std::env::args_os().skip(1))? {
        cli::Command::Help => {
            print_usage();
            return Ok(());
        }
        cli::Command::Run(args) => args,
        cli::Command::Init { name } => return init_vault(tty, &name),
        cli::Command::Recover { name } => return recover_vault(tty, &name),
        cli::Command::CheckCode { name } => return check_code(tty, &name),
        cli::Command::Rm { name } => return remove_vault(tty, &name),
    };
    let home = harden::env_var(harden::EnvVar::Home);
    // Step 7. Opening never creates the data directory (only `crystal
    // init` does); the vault itself is opened at unlock (step 8).
    let data_dir = vault::DataDir::open(home.as_deref())?;
    let state_dir = vault::StateDir::open_or_create(home.as_deref())?;
    // Last, so every earlier error prints on the normal screen. Dropped when
    // startup returns, before main prints any error.
    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    #[cfg(debug_assertions)]
    if let Some(kind) = args.crash_for_test {
        crash_for_test(kind);
    }
    #[cfg(debug_assertions)]
    if let Some(kind) = args.prompt_for_test {
        let pass = match kind {
            cli::PromptKind::Read => terminal.read_passphrase(&signals, PROMPT)?,
            cli::PromptKind::Confirm => terminal.read_new_passphrase(&signals, PROMPT, REPEAT)?,
        };
        // Leave the alternate screen first, so the line stays visible.
        drop(terminal);
        print_len_for_test(pass.len_for_test());
        return Ok(());
    }
    #[cfg(debug_assertions)]
    if args.skip_unlock_for_test {
        terminal.run_until_exit(&signals)?;
        return Ok(());
    }

    // Step 8: unlock. The vault directory is checked before the prompt, so
    // a mistyped name fails at once.
    let mut vault_dir = unlock::open_vault_dir(&data_dir, &args.name)?;
    drop(data_dir);
    let mut state_dir = state_dir;
    // Each turn unlocks and runs a session; `lock` ends the session, which
    // gives back only the two directory handles, and the next turn shows
    // the lock screen (docs/decisions/0048-lock-and-auto-lock.md).
    let mut locked: Option<app::LockReason> = None;
    loop {
        let unlocked = match locked {
            // At start, one try (decision 0023): a wrong passphrase exits.
            // unlock takes the passphrase and wipes it as soon as
            // identity.age is open, before vault.age is read.
            None => {
                let pass = terminal.read_passphrase(&signals, PROMPT)?;
                unlock::unlock(&vault_dir, &args.name, pass, &state_dir)?
            }
            Some(reason) => {
                let again = unlock_again(
                    &mut terminal,
                    &signals,
                    &vault_dir,
                    &args.name,
                    &state_dir,
                    reason_line(reason),
                )?;
                let Some(unlocked) = again else {
                    return Ok(());
                };
                unlocked
            }
        };
        match run_unlocked(
            &mut terminal,
            &signals,
            &args,
            unlocked,
            vault_dir,
            state_dir,
        )? {
            Some((session, reason)) => {
                (vault_dir, state_dir) = session.into_dirs();
                // Everything from the vault is wiped by now: only then
                // does a SIGTSTP stop the process (decision 0048).
                if let app::LockReason::Suspended(_) = reason {
                    terminal.suspend()?;
                }
                locked = Some(reason);
            }
            None => return Ok(()),
        }
    }
}

/// The lock screen's cow (cowsay's default cow, GPL like Crystal; decision
/// 0048). Fixed text: nothing from the vault is shown while locked.
const COW: [&str; 8] = [
    r"/ I locked your vault to protect you \",
    r"\ while you were away.               /",
    r" ------------------------------------",
    r"        \   (__)",
    r"         \  (oo)\_______",
    r"            (__)\       )\/\",
    r"                ||----w |",
    r"                ||     ||",
];

const LOCKED_BY_COMMAND: &str =
    "Locked by the lock command; notes, history and clipboard were wiped from memory.";
const LOCKED_IDLE: &str = "Locked after 5 minutes idle; notes and history were wiped from memory.";
const LOCKED_IDLE_SAVED: &str = "Locked after 5 minutes idle; your unsaved edits were saved first.";
const LOCKED_SUSPENDED: &str =
    "Locked when Crystal was suspended; notes and history were wiped from memory.";
const LOCKED_SUSPENDED_SAVED: &str =
    "Locked when Crystal was suspended; your unsaved edits were saved first.";
const LOCKED_SUSPENDED_LOST: &str =
    "Locked when suspended. Your edits could NOT be saved: they are lost.";
const LOCKED_IDLE_LOST: &str =
    "Locked after 5 minutes idle. Your edits could NOT be saved: they are lost.";

/// The lock screen's line for why Crystal locked.
fn reason_line(reason: app::LockReason) -> &'static str {
    match reason {
        app::LockReason::Command => LOCKED_BY_COMMAND,
        app::LockReason::Idle(app::Edits::None) => LOCKED_IDLE,
        app::LockReason::Idle(app::Edits::Saved) => LOCKED_IDLE_SAVED,
        app::LockReason::Idle(app::Edits::Lost) => LOCKED_IDLE_LOST,
        app::LockReason::Suspended(app::Edits::None) => LOCKED_SUSPENDED,
        app::LockReason::Suspended(app::Edits::Saved) => LOCKED_SUSPENDED_SAVED,
        app::LockReason::Suspended(app::Edits::Lost) => LOCKED_SUSPENDED_LOST,
    }
}
const WRONG_PASSPHRASE: &str = "Wrong passphrase; try again.";

/// The lock screen: the cow, why Crystal locked, a note (a wrong
/// passphrase) and the prompt. Raw mode: lines end in CR LF.
fn lock_screen(reason: &str, note: &str) -> String {
    let mut screen = COW.join("\r\n");
    for line in ["", "", reason, note, "Passphrase: "] {
        screen.push_str(line);
        if !line.starts_with("Passphrase") {
            screen.push_str("\r\n");
        }
    }
    screen
}

/// After a lock (decision 0048): asks on the lock screen until the
/// passphrase opens the vault; a wrong one asks again (each try costs
/// scrypt's second). `None` when the user leaves (Esc, Ctrl+C, an exit
/// signal, a closed terminal): Crystal exits, with nothing left to wipe.
/// Any other failure (the vault changed into something that does not
/// open) ends Crystal with its error, as at start.
fn unlock_again(
    terminal: &mut term::TerminalGuard,
    signals: &term::Signals,
    vault_dir: &vault::vault_dir::VaultDir,
    name: &vault::VaultName,
    state_dir: &vault::StateDir,
    reason: &str,
) -> Result<Option<unlock::Unlocked>, Box<dyn Error>> {
    let mut note = "";
    loop {
        let pass = match terminal.read_passphrase(signals, &lock_screen(reason, note)) {
            Ok(pass) => pass,
            Err(term::PromptError::Cancelled | term::PromptError::Closed) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match unlock::unlock(vault_dir, name, pass, state_dir) {
            Ok(unlocked) => return Ok(Some(unlocked)),
            Err(unlock::UnlockError::Identity(crypto::CryptoError::WrongPassphrase)) => {
                note = WRONG_PASSPHRASE;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// An unlocked vault's session: the restore question, the shell and the
/// app. Returns the session and why it locked, for its directory handles
/// and the lock screen; `None`
/// when Crystal exits. The shell (scrollback, history) is dropped here
/// either way, and with it everything shown (all wiped).
fn run_unlocked(
    terminal: &mut term::TerminalGuard,
    signals: &term::Signals,
    args: &cli::RunArgs,
    unlocked: unlock::Unlocked,
    vault_dir: vault::vault_dir::VaultDir,
    state_dir: vault::StateDir,
) -> Result<Option<(session::Session, app::LockReason)>, Box<dyn Error>> {
    // An older vault than this device has seen: restored, or put back by
    // someone else. Only the word `restore` accepts it (decision 0036);
    // anything else drops the notes and changes nothing.
    let restored = if let Some(older) = unlocked.older {
        confirm_restore(terminal, signals, older)?;
        unlock::accept_restore(&state_dir, unlocked.contents.vault_id, older)?;
        "\nThe restored vault was accepted: rollback detection starts again from it."
    } else {
        ""
    };

    // The shell (docs/decisions/0033-shell.md).
    let first = if unlocked.pinned == vault::pin::Pinned::New
        || unlocked.seen == vault::generation::Seen::FirstUse
    {
        "\nFirst unlock of this vault on this device: it is pinned to its name, and rollback \
         detection starts now."
    } else {
        ""
    };
    let mut shell = shell::Shell::new(&args.name);
    #[cfg_attr(not(debug_assertions), expect(unused_mut, reason = "debug test hook"))]
    let mut contents = unlocked.contents;
    #[cfg(debug_assertions)]
    if args.demo_notes_for_test {
        demo_notes_for_test(&mut contents);
    }
    #[cfg(debug_assertions)]
    let demo = args.demo_notes_for_test;
    #[cfg(not(debug_assertions))]
    let demo = false;
    let mut session = session::Session::new(
        vault_dir,
        state_dir,
        unlocked.recipient,
        unlocked.stamp,
        notes::Notes::new(contents),
    );
    shell.set_names(app::names(session.notes()));
    let count = session.notes().len();
    shell.print(
        &format!("{count} notes unlocked.{first}{restored}"),
        ui::grid::Style::PLAIN,
    );
    shell.print("Type help for the commands.", ui::grid::Style::PLAIN);
    #[cfg(debug_assertions)]
    let idle = args
        .lock_after_for_test
        .map_or(app::AUTO_LOCK, std::time::Duration::from_secs);
    #[cfg(not(debug_assertions))]
    let idle = app::AUTO_LOCK;
    let outcome = app::run(terminal, signals, &mut shell, &mut session, demo, idle)?;
    drop(shell);
    match outcome {
        app::Outcome::Lock(reason) => Ok(Some((session, reason))),
        // The notes, the recipient and the vault and state fds.
        app::Outcome::Quit => Ok(None),
    }
}

const PROMPT: &str = "Passphrase: ";
const NEW_PROMPT: &str = "Crystal recommends a passphrase of 8 or more random words, generated \
                          with KeePassDX or KeePassXC.\r\n\r\nNew passphrase for this \
                          vault: ";
const REPEAT: &str = "Repeat passphrase: ";

/// HOME, which the data and state directory checks require to be set and
/// absolute.
fn home_dir() -> Result<std::ffi::OsString, Box<dyn Error>> {
    harden::env_var(harden::EnvVar::Home).ok_or_else(|| "HOME is unset".into())
}

/// `crystal init NAME`: create a vault in ~/.local/share/crystal, then exit
/// (docs/design/storage.md, "Key design"). The name is checked first; then
/// a new passphrase is asked for twice, and the new recovery code is shown
/// and typed back; only then is anything created, and a failure removes it
/// again.
fn init_vault(tty: term::Tty, name: &vault::VaultName) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    let data_dir = vault::DataDir::create(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    let plan = init::plan(&data_dir, name)?;

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let pass = terminal.read_new_passphrase(&signals, NEW_PROMPT, REPEAT)?;
    let recovery = recovery::RecoveryCode::generate().map_err(init::InitError::Random)?;
    init::confirm_recovery_code(&mut terminal, &signals, &recovery)?;
    drop(terminal);
    init::create(plan, name, &pass, &recovery, &data_dir, &state_dir)?;
    Ok(())
}

/// `crystal recover NAME` (docs/decisions/0029-recovery-code.md): the
/// recovery code opens the vault's key; once the vault is checked against
/// its pin and rollback counter, a new passphrase seals the key into a new
/// identity.age. Nothing else changes.
fn recover_vault(tty: term::Tty, name: &vault::VaultName) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    let data_dir = vault::DataDir::open(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    let vault_dir = vault::vault_dir::VaultDir::open(&data_dir, name)?;

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let opened = open_with_code(&mut terminal, &signals, &vault_dir)?;
    if let Some(older) = recover::check_seen(&opened, name, &state_dir)? {
        confirm_restore(&mut terminal, &signals, older)?;
        recover::accept_restore(&opened, &state_dir, older)?;
    }
    let pass = terminal.read_new_passphrase(&signals, NEW_PROMPT, REPEAT)?;
    drop(terminal);
    recover::finish(opened, &pass, &vault_dir)?;
    Ok(())
}

/// `crystal check-code NAME`: the recovery code must open the backup, and
/// the backup must hold the same key as identity.age. Writes nothing.
fn check_code(tty: term::Tty, name: &vault::VaultName) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    let data_dir = vault::DataDir::open(Some(&home))?;
    let vault_dir = vault::vault_dir::VaultDir::open(&data_dir, name)?;

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let opened = open_with_code(&mut terminal, &signals, &vault_dir)?;
    let pass = terminal.read_passphrase(&signals, PROMPT)?;
    drop(terminal);
    recover::check_code(&opened, &pass, &vault_dir)?;
    print_done(
        &"The recovery code is right: it opens identity.backup.age, which holds the same \
                 key as identity.age.",
    );
    Ok(())
}

/// Asks whether an older vault is a restore; anything but `restore` is a
/// refusal, and nothing has been changed yet.
fn confirm_restore(
    terminal: &mut term::TerminalGuard,
    signals: &term::Signals,
    older: vault::generation::Older,
) -> Result<(), Box<dyn Error>> {
    let answer = terminal.read_public_line(signals, &unlock::restore_prompt(older))?;
    if answer.as_str() == unlock::RESTORE_WORD {
        Ok(())
    } else {
        Err(unlock::UnlockError::NotRestored.into())
    }
}

/// Asks for the recovery code and opens the vault's backup with it.
fn open_with_code(
    terminal: &mut term::TerminalGuard,
    signals: &term::Signals,
    vault_dir: &vault::vault_dir::VaultDir,
) -> Result<recover::Opened, Box<dyn Error>> {
    let typed = terminal.read_public_line(signals, recover::CODE_PROMPT)?;
    let code = recovery::RecoveryCode::parse(&typed).map_err(recover::RecoverError::Code)?;
    drop(typed);
    Ok(recover::open_backup(vault_dir, &code)?)
}

/// `crystal rm NAME`: delete a vault for good once its name is typed again
/// (docs/decisions/0032-fixed-locations.md).
fn remove_vault(tty: term::Tty, name: &vault::VaultName) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    let data_dir = vault::DataDir::open(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    // Checked before the prompt, so a mistyped name fails at once.
    drop(vault::vault_dir::VaultDir::open_for_removal(
        &data_dir, name,
    )?);

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let typed = terminal.read_public_line(&signals, rm::CONFIRM_PROMPT)?;
    drop(terminal);
    if typed.as_str() != name.as_str() {
        return Err(rm::RmError::NotConfirmed.into());
    }
    let removed = rm::remove(&data_dir, &state_dir, name)?;
    print_done(&removed);
    Ok(())
}

/// Debug builds only: notes for the tests through the binary, in memory
/// only (`--demo-notes-for-test`). One of them is hostile.
#[cfg(debug_assertions)]
fn demo_notes_for_test(contents: &mut vault::codec::VaultContents) {
    let text = |s: &str| model::NoteText::from_zeroizing(zeroize::Zeroizing::new(s.to_owned()));
    let notes = [
        (
            "",
            "welcome",
            "# Welcome\n\nSee [[guide#Keys]] and #demo.\n\nsecret-demo-text",
        ),
        ("docs", "guide", "# Guide\n\n## Keys\n\nPress q. ^keys"),
        (
            "",
            "hostile",
            "a\u{1b}]52;c;ZXZpbA==\u{7}b \u{1b}[2J \u{202E}z",
        ),
    ];
    for (i, (folder, title, body)) in notes.into_iter().enumerate() {
        contents.notes.push(model::Note {
            id: model::NoteId::from_bytes([u8::try_from(i).unwrap_or(0) + 1; 16]),
            folder: text(folder),
            title: text(title),
            created: 0,
            modified: 0,
            body: text(body),
        });
    }
}

/// Debug builds only: the prompt tests check the length of what was
/// entered. Never the content.
#[cfg(debug_assertions)]
#[expect(clippy::disallowed_macros, reason = "debug-only test output, a number")]
fn print_len_for_test(len: usize) {
    println!("len={len}");
}

/// Debug builds only: the abrupt exits tests/startup.rs checks. Every
/// message carries a fake secret, which must never be printed.
#[cfg(debug_assertions)]
fn crash_for_test(kind: cli::CrashKind) {
    match kind {
        cli::CrashKind::Panic => panic!("forced test panic with note text: hunter2"),
        cli::CrashKind::PanicInDrop => {
            struct PanicOnDrop;
            impl Drop for PanicOnDrop {
                fn drop(&mut self) {
                    panic!("panic in Drop with note text: hunter2");
                }
            }
            let _dropped_while_unwinding = PanicOnDrop;
            panic!("forced test panic with note text: hunter2");
        }
        cli::CrashKind::StackOverflow => {
            std::hint::black_box(recurse_forever(0));
        }
    }
}

/// black_box keeps the compiler from turning the recursion into a loop.
#[cfg(debug_assertions)]
#[expect(unconditional_recursion, reason = "the point: overflow the stack")]
fn recurse_forever(depth: u64) -> u64 {
    let frame = std::hint::black_box([depth; 64]);
    recurse_forever(frame[0] + 1) + frame[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock screen is written to the terminal as is, not through the
    /// grid: only printable ASCII and CR LF, no line wider than 80
    /// columns, the cow first and the prompt last.
    #[test]
    fn lock_screen_is_plain_fixed_text() {
        let reasons = [
            app::LockReason::Command,
            app::LockReason::Idle(app::Edits::None),
            app::LockReason::Idle(app::Edits::Saved),
            app::LockReason::Idle(app::Edits::Lost),
            app::LockReason::Suspended(app::Edits::None),
            app::LockReason::Suspended(app::Edits::Saved),
            app::LockReason::Suspended(app::Edits::Lost),
        ];
        for (reason, note) in reasons
            .into_iter()
            .flat_map(|r| [(r, ""), (r, WRONG_PASSPHRASE)])
        {
            let line = reason_line(reason);
            let screen = lock_screen(line, note);
            assert!(
                screen
                    .bytes()
                    .all(|b| (0x20..0x7f).contains(&b) || b == b'\r' || b == b'\n'),
                "{screen:?}"
            );
            assert!(screen.split("\r\n").all(|line| line.len() <= 80));
            assert!(!screen.replace("\r\n", "").contains(['\r', '\n']));
            assert!(screen.starts_with(COW[0]));
            assert!(screen.ends_with("\r\nPassphrase: "));
            assert!(screen.contains(line));
            assert_eq!(screen.contains(WRONG_PASSPHRASE), !note.is_empty());
        }
    }
}
