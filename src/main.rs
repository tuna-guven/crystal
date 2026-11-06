//! Crystal: an encrypted Markdown vault in a TUI.
//!
//! See docs/DESIGN.md for the architecture and threat model.

#![forbid(unsafe_code)]

mod backup;
mod cli;
mod crypto;
mod harden;
mod md;
mod model;
mod sys;
mod term;
mod ui;
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

/// DESIGN.md startup order, steps 2 onward. Each step fails closed.
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
    };
    if args.init {
        // Fail before creating or opening anything; Phase 2 replaces this.
        return Err("--init is not implemented yet (Phase 2)".into());
    }
    let home = harden::env_var(harden::EnvVar::Home);
    // The root fd lives only in this block: once the vault is open it is
    // closed, so other vaults are unreachable for the rest of the session.
    let _vault_dir = {
        let vaults = vault::VaultsRoot::open(home.as_deref())?;
        vault::VaultDir::open(&vaults, &args.vault_name)?
    };
    // After the vault, so a mistyped name creates nothing.
    let _state_dir = vault::StateDir::open_or_create(home.as_deref())?;
    // Last, so every earlier error prints on the normal screen. Dropped when
    // startup returns, before main prints any error.
    let signals = term::Signals::register()?;
    let mut terminal = term::TerminalGuard::enter(tty)?;
    #[cfg(debug_assertions)]
    if let Some(kind) = args.crash_for_test {
        crash_for_test(kind);
    }
    // Phase 1 placeholder for the TUI: Ctrl+Z suspends, any other key or
    // an exit signal returns, so the guard restores the terminal.
    terminal.run_until_exit(&signals)?;
    Ok(())
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
