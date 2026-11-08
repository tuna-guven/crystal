//! Crystal: an encrypted Markdown vault in a TUI.
//!
//! See the book in docs/ (docs/SUMMARY.md) for the architecture and threat
//! model.

// The real program has no unsafe code, and nothing can switch this off.
#![cfg_attr(not(test), forbid(unsafe_code))]
// Test builds only: still an error by default, but a #[cfg(test)] function
// that checks memory (zeroization) may allow it, with a SAFETY comment.
#![cfg_attr(test, deny(unsafe_code))]

mod backup;
mod cli;
mod config;
mod crypto;
mod harden;
mod init;
mod md;
mod model;
mod recover;
mod recovery;
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
        cli::Command::Init { name, at } => return init_vault(tty, &name, at.as_ref()),
        cli::Command::Add { name, dir } => return add_vault(tty, &name, &dir),
        cli::Command::Remove { name } => return remove_vault(tty, &name),
        cli::Command::Recover { name, dir } => return recover_vault(tty, &name, &dir),
    };
    let home = harden::env_var(harden::EnvVar::Home);
    // Step 7. Opening never creates the config directory (only `crystal init` and
    // `crystal add` do). The vault is looked up by name in config.age at unlock
    // (step 8); ~/Vaults is never opened directly.
    let config_dir = vault::ConfigDir::open(home.as_deref())?;
    // After the config, so a run without one creates nothing.
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

    // Step 8: unlock. Without a config there is nothing to ask for.
    if !config::exists(&config_dir)? {
        return Err(unlock::UnlockError::NoConfig.into());
    }
    let pass = terminal.read_passphrase(&signals, PROMPT)?;
    // unlock takes the passphrase and wipes it as soon as identity.age is
    // open, before vault.age is read.
    let unlocked = unlock::unlock(&args.name, pass, &home_dir()?, &config_dir, &state_dir)?;
    // The sandbox never reaches the config: its fd is closed now. The vault
    // and state fds stay open (Landlock rule fds in Phase 6).
    drop(config_dir);

    // Phase 2 placeholder for the TUI (Phase 3): the note count only,
    // never a title.
    let notes = unlocked.contents.notes.len();
    let first = if unlocked.seen == vault::generation::Seen::FirstUse {
        "\r\nFirst unlock of this vault on this device: rollback detection starts now."
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
                          device's Crystal config: ";
const REPEAT: &str = "Repeat passphrase: ";

/// HOME, which the config and state directory checks have already
/// required to be set and absolute.
fn home_dir() -> Result<std::ffi::OsString, Box<dyn Error>> {
    harden::env_var(harden::EnvVar::Home).ok_or_else(|| "HOME is unset".into())
}

/// `crystal add NAME DIR`: register an existing vault, then exit. The directory
/// is resolved and checked before any prompt, so a bad path fails at once
/// and writes nothing. Then the config is unlocked (or, on a device
/// without one, created with a new passphrase), the vault's identity.age
/// and vault.age are opened with the same passphrase to pin its vault_id,
/// and the config is stored.
fn add_vault(
    tty: term::Tty,
    name: &vault::VaultName,
    dir: &cli::ArgPath,
) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    let config_dir = vault::ConfigDir::create(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    let resolved = resolve_vault_dir(dir)?;
    let vault_dir = vault::vault_dir::VaultDir::open_checked(
        resolved.as_ref(),
        &home,
        &config_dir,
        &state_dir,
    )?;
    let has_config = config::exists(&config_dir)?;

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let (pass, mut cfg) = if has_config {
        let pass = terminal.read_passphrase(&signals, PROMPT)?;
        let (cfg, _) =
            config::load(&config_dir, &state_dir, &pass)?.ok_or(config::ConfigError::Missing)?;
        (pass, cfg)
    } else {
        let pass = terminal.read_new_passphrase(&signals, NEW_PROMPT, REPEAT)?;
        (pass, config::fresh(&state_dir)?)
    };
    drop(terminal);

    if cfg.find(name).is_some() {
        return Err(config::ConfigError::NameTaken.into());
    }
    let (vault_id, vault_generation) =
        config::identify_vault(&vault_dir, &pass).map_err(|e| match e {
            config::ConfigError::Vault(vault::format::VaultError::Crypto(
                crypto::CryptoError::WrongPassphrase,
            )) => config::ConfigError::VaultPassphrase {
                new_device: !has_config,
            },
            e => e,
        })?;
    // Refuses a taken name or a vault already registered, before anything
    // is written.
    cfg.add(config::VaultEntry {
        name: vault::VaultName::new(name.as_str().into()).ok_or(config::ConfigError::BadName)?,
        path: resolved,
        vault_id,
        lock_after: None,
    })?;
    // The vault's rollback baseline on this device starts here, and an
    // older vault than this device has seen is refused. Recorded before
    // the config is stored: the counter means "seen here", and this
    // generation was just decrypted and authenticated, whether or not the
    // config write that follows succeeds.
    vault::generation::accept_on_open(
        state_dir.fd(),
        vault::generation::Counter::Vault(vault_id),
        vault_generation,
    )?;
    config::store(&mut cfg, &config_dir, &state_dir, &pass)?;
    Ok(())
}

/// DIR fully resolved, as the config stores it: the vault check refuses
/// any symlink (/home is one on Fedora Atomic). For `add` and `recover`.
fn resolve_vault_dir(dir: &cli::ArgPath) -> Result<String, Box<dyn Error>> {
    let resolved = std::fs::canonicalize(dir.as_path())
        .map_err(|e| format!("cannot resolve the vault directory ({})", e.kind()))?;
    Ok(resolved
        .into_os_string()
        .into_string()
        .map_err(|_| "the vault directory's path must be valid UTF-8")?)
}

/// `crystal recover NAME DIR` (docs/decisions/0029-recovery-code.md): the
/// vault in DIR is opened with its recovery code; its key is sealed again
/// with this device's passphrase and the vault is registered as NAME. On a
/// device whose config opens only with a forgotten passphrase, that config
/// is replaced once the user types `replace`. DIR is checked before any
/// prompt, as for `crystal add`.
fn recover_vault(
    tty: term::Tty,
    name: &vault::VaultName,
    dir: &cli::ArgPath,
) -> Result<(), Box<dyn Error>> {
    use recover::RecoverError;
    let home = home_dir()?;
    let config_dir = vault::ConfigDir::create(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    let resolved = resolve_vault_dir(dir)?;
    let vault_dir = vault::vault_dir::VaultDir::open_checked(
        resolved.as_ref(),
        &home,
        &config_dir,
        &state_dir,
    )?;
    let has_config = config::exists(&config_dir)?;

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let typed = terminal.read_public_line(&signals, recover::CODE_PROMPT)?;
    let code = recovery::RecoveryCode::parse(&typed).map_err(RecoverError::Code)?;
    drop(typed);
    let opened = recover::open_backup(&vault_dir, &code)?;
    drop(code);

    let device_pass = if has_config {
        terminal.read_passphrase_or_empty(&signals, recover::DEVICE_PROMPT)?
    } else {
        None
    };
    let (pass, cfg) = match device_pass {
        Some(pass) => {
            let (cfg, _) = config::load(&config_dir, &state_dir, &pass)?
                .ok_or(config::ConfigError::Missing)?;
            (pass, cfg)
        }
        None => {
            if has_config {
                let answer = terminal.read_public_line(&signals, recover::REPLACE_PROMPT)?;
                if answer.as_str() != recover::REPLACE_WORD {
                    return Err(RecoverError::NotReplaced.into());
                }
            }
            let pass = terminal.read_new_passphrase(&signals, NEW_PROMPT, REPEAT)?;
            (pass, config::fresh(&state_dir)?)
        }
    };
    drop(terminal);
    recover::finish(
        opened,
        name,
        resolved,
        &pass,
        cfg,
        &vault_dir,
        &config_dir,
        &state_dir,
    )?;
    Ok(())
}

/// `crystal init NAME [--at DIR]`: create a vault and register it, then exit
/// (docs/design/storage.md, "Key design"). The target is resolved and
/// checked first; then the passphrase (the config's, or a new one twice on
/// a device without a config) is asked for, and the new recovery code is
/// shown and typed back; only then is anything created, and a failure
/// removes it again.
fn init_vault(
    tty: term::Tty,
    name: &vault::VaultName,
    at: Option<&cli::ArgPath>,
) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    let config_dir = vault::ConfigDir::create(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    let plan = init::plan(
        &home,
        at.map(cli::ArgPath::as_path),
        name,
        &config_dir,
        &state_dir,
    )?;
    let has_config = config::exists(&config_dir)?;

    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let (pass, cfg) = if has_config {
        let pass = terminal.read_passphrase(&signals, PROMPT)?;
        let (cfg, _) =
            config::load(&config_dir, &state_dir, &pass)?.ok_or(config::ConfigError::Missing)?;
        (pass, cfg)
    } else {
        let pass = terminal.read_new_passphrase(&signals, NEW_PROMPT, REPEAT)?;
        (pass, config::fresh(&state_dir)?)
    };
    if cfg.find(name).is_some() {
        return Err(config::ConfigError::NameTaken.into());
    }
    let recovery = recovery::RecoveryCode::generate().map_err(init::InitError::Random)?;
    init::confirm_recovery_code(&mut terminal, &signals, &recovery)?;
    drop(terminal);
    init::create(
        plan,
        name,
        &pass,
        &recovery,
        cfg,
        &home,
        &config_dir,
        &state_dir,
    )?;
    Ok(())
}

/// `crystal remove NAME`: unlock the config, forget NAME, store, exit. The
/// vault's own files are never touched.
fn remove_vault(tty: term::Tty, name: &vault::VaultName) -> Result<(), Box<dyn Error>> {
    let home = home_dir()?;
    // Never creates anything: without a config there is nothing to remove.
    let config_dir = vault::ConfigDir::open(Some(&home))?;
    let state_dir = vault::StateDir::open_or_create(Some(&home))?;
    if !config::exists(&config_dir)? {
        return Err(config::ConfigError::UnknownName.into());
    }
    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    let pass = terminal.read_passphrase(&signals, PROMPT)?;
    drop(terminal);
    let (mut cfg, _) =
        config::load(&config_dir, &state_dir, &pass)?.ok_or(config::ConfigError::Missing)?;
    cfg.remove(name)?;
    config::store(&mut cfg, &config_dir, &state_dir, &pass)?;
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
