//! Crystal: an encrypted Markdown vault in a terminal shell.
//!
//! See the book in docs/ (docs/SUMMARY.md) for the architecture and threat
//! model.

// The real program has no unsafe code, and nothing can switch this off.
#![cfg_attr(not(test), forbid(unsafe_code))]
// Test builds only: still an error by default, but a #[cfg(test)] function
// that checks memory (zeroization) may allow it, with a SAFETY comment.
#![cfg_attr(test, deny(unsafe_code))]

mod cli;
mod crypto;
mod harden;
mod init;
mod md;
mod model;
mod recover;
mod recovery;
mod rm;
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
    let vault_dir = unlock::open_vault_dir(&data_dir, &args.name)?;
    drop(data_dir);
    let pass = terminal.read_passphrase(&signals, PROMPT)?;
    // unlock takes the passphrase and wipes it as soon as identity.age is
    // open, before vault.age is read.
    let unlocked = unlock::unlock(vault_dir, &args.name, pass, &state_dir)?;

    // Phase 2 placeholder for the shell (Phase 3): the note count only,
    // never a title.
    let notes = unlocked.contents.notes.len();
    let first = if unlocked.pinned == vault::pin::Pinned::New
        || unlocked.seen == vault::generation::Seen::FirstUse
    {
        "\r\nFirst unlock of this vault on this device: it is pinned to its name, and \
         rollback detection starts now."
    } else {
        ""
    };
    terminal.show_status(&format!(
        "{notes} notes unlocked.{first}\r\nPress any key to lock and quit."
    ))?;
    // Ctrl+Z suspends, any other key or an exit signal returns, so the
    // guard restores the terminal and the notes are dropped (wiped).
    terminal.run_until_exit(&signals)?;
    drop(unlocked);
    drop(state_dir);
    Ok(())
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
    recover::check_seen(&opened, name, &state_dir)?;
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
