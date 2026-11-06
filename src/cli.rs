//! Command-line arguments (startup step 6). Never carries secrets: argv is
//! readable by other processes through /proc/<pid>/cmdline. Errors never
//! echo argument text, since a mistyped argument could be a passphrase.

use lexopt::{Arg, Parser, ValueExt};
#[cfg(debug_assertions)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;

use crate::vault::VaultName;

pub const USAGE: &str =
    "usage: crystal [--init] [--lock-after MINUTES] VAULT_NAME\nvaults live in ~/Vaults";

const DEFAULT_LOCK_AFTER_MINUTES: u32 = 5;
const MAX_LOCK_AFTER_MINUTES: u32 = 24 * 60;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Args),
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub vault_name: VaultName,
    pub init: bool,
    pub lock_after_minutes: u32,
    /// Debug builds only: crash after entering the alternate screen, for
    /// the tests of abrupt exits. Not compiled into release builds.
    #[cfg(debug_assertions)]
    pub crash_for_test: Option<CrashKind>,
}

/// How `--crash-for-test KIND` ends the process. Debug builds only.
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashKind {
    /// `panic`: unwinds; the panic hook restores the terminal, Drop runs.
    Panic,
    /// `panic-in-drop`: a second panic inside Drop while unwinding. Rust
    /// aborts after the hook ran; the remaining Drop impls are skipped.
    PanicInDrop,
    /// `stack-overflow`: on musl, a bare SIGSEGV (exit 139) that prints
    /// nothing. No hook and no Drop run.
    StackOverflow,
}

/// Fixed messages only; never the offending argument.
#[derive(Debug, PartialEq, Eq)]
pub enum CliError {
    UnknownArgument,
    MissingVault,
    ExtraVault,
    BadVaultName,
    BadLockAfter,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::UnknownArgument => "unknown argument",
            Self::MissingVault => "missing vault name",
            Self::ExtraVault => "more than one vault name given",
            Self::BadVaultName => {
                "vault name must be 1 to 64 letters, digits, - or _, starting with a letter or digit"
            }
            Self::BadLockAfter => "--lock-after needs whole minutes from 1 to 1440",
        };
        write!(f, "{msg}\n{USAGE}")
    }
}

impl std::error::Error for CliError {}

/// Parses the arguments after the program name. lexopt's own errors are
/// discarded because they quote the argument.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let mut parser = Parser::from_args(args);
    let mut init = false;
    let mut lock_after_minutes = DEFAULT_LOCK_AFTER_MINUTES;
    let mut vault_name = None;
    #[cfg(debug_assertions)]
    let mut crash_for_test = None;

    while let Some(arg) = parser.next().map_err(|_| CliError::UnknownArgument)? {
        match arg {
            Arg::Long("init") => init = true,
            #[cfg(debug_assertions)]
            Arg::Long("crash-for-test") => {
                let kind = match parser.value().ok().as_deref().and_then(OsStr::to_str) {
                    Some("panic") => CrashKind::Panic,
                    Some("panic-in-drop") => CrashKind::PanicInDrop,
                    Some("stack-overflow") => CrashKind::StackOverflow,
                    _ => return Err(CliError::UnknownArgument),
                };
                crash_for_test = Some(kind);
            }
            Arg::Long("lock-after") => {
                lock_after_minutes = parser
                    .value()
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                    .filter(|m| (1..=MAX_LOCK_AFTER_MINUTES).contains(m))
                    .ok_or(CliError::BadLockAfter)?;
            }
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help),
            Arg::Value(name) if vault_name.is_none() => {
                vault_name = Some(VaultName::new(name).ok_or(CliError::BadVaultName)?);
            }
            Arg::Value(_) => return Err(CliError::ExtraVault),
            _ => return Err(CliError::UnknownArgument),
        }
    }

    Ok(Command::Run(Args {
        vault_name: vault_name.ok_or(CliError::MissingVault)?,
        init,
        lock_after_minutes,
        #[cfg(debug_assertions)]
        crash_for_test,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, CliError> {
        parse(args.iter().map(OsString::from))
    }

    fn run(vault_name: &str, init: bool, lock_after_minutes: u32) -> Command {
        Command::Run(Args {
            vault_name: VaultName::new(OsString::from(vault_name)).unwrap(),
            init,
            lock_after_minutes,
            #[cfg(debug_assertions)]
            crash_for_test: None,
        })
    }

    #[test]
    fn defaults() {
        assert_eq!(parse_strs(&["v"]), Ok(run("v", false, 5)));
    }

    #[test]
    fn all_options_in_any_order() {
        let want = Ok(run("v", true, 30));
        assert_eq!(parse_strs(&["--init", "--lock-after", "30", "v"]), want);
        assert_eq!(parse_strs(&["v", "--lock-after=30", "--init"]), want);
    }

    #[test]
    fn help() {
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h", "v"]), Ok(Command::Help));
    }

    #[test]
    fn vault_name_is_required_unique_and_valid() {
        assert_eq!(parse_strs(&[]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["--init"]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["a", "b"]), Err(CliError::ExtraVault));
        assert_eq!(parse_strs(&["../x"]), Err(CliError::BadVaultName));
    }

    #[test]
    fn lock_after_bounds() {
        let lock_after = |m: &str| parse_strs(&["--lock-after", m, "v"]);
        assert_eq!(lock_after("1"), Ok(run("v", false, 1)));
        assert_eq!(lock_after("1440"), Ok(run("v", false, 1440)));
        for bad in ["0", "1441", "-1", "1.5", "five", ""] {
            assert_eq!(lock_after(bad), Err(CliError::BadLockAfter), "{bad}");
        }
        assert_eq!(
            parse_strs(&["v", "--lock-after"]),
            Err(CliError::BadLockAfter)
        );
    }

    // A mistyped secret must never be echoed back to the terminal.
    #[test]
    fn errors_never_echo_arguments() {
        let attempts: [&[&str]; 6] = [
            &["--passphrase=hunter2", "v"],
            &["--passphrase", "hunter2", "v"],
            &["--init=hunter2", "v"],
            &["--lock-after", "hunter2", "v"],
            &["v", "hunter2"],
            &["hunter2/../x"],
        ];
        for args in attempts {
            let err = parse_strs(args).unwrap_err();
            assert!(!err.to_string().contains("hunter2"), "{args:?}");
            assert!(!format!("{err:?}").contains("hunter2"), "{args:?}");
        }
    }

    // A passphrase typed in place of the vault name is a valid name; the
    // parsed arguments must still never show it.
    #[test]
    fn parsed_args_never_show_the_vault_name() {
        let parsed = parse_strs(&["hunter2"]).unwrap();
        assert!(!format!("{parsed:?}").contains("hunter2"), "{parsed:?}");
    }
}
