//! Command-line arguments (startup step 6). Never carries secrets: argv is
//! readable by other processes through /proc/<pid>/cmdline. Errors never
//! echo argument text, since a mistyped argument could be a passphrase.

use lexopt::{Arg, Parser, ValueExt};
#[cfg(debug_assertions)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use crate::vault::VaultName;

pub const USAGE: &str = "\
usage: crystal [--lock-after MINUTES] NAME   open the vault NAME
       crystal --init NAME [--at DIR]        create a vault in ~/Vaults/NAME or in DIR
       crystal --add NAME DIR                register an existing vault as NAME
       crystal --remove NAME                 forget NAME; its files are kept
       crystal --help";

const MAX_LOCK_AFTER_MINUTES: u32 = 24 * 60;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `crystal NAME`: open a vault registered in config.age.
    Run(RunArgs),
    /// `--init NAME [--at DIR]`: create a vault and register it.
    Init {
        name: VaultName,
        at: Option<ArgPath>,
    },
    /// `--add NAME DIR`: register an existing vault.
    Add {
        name: VaultName,
        dir: ArgPath,
    },
    /// `--remove NAME`: drop a vault from the config; no files are deleted.
    Remove {
        name: VaultName,
    },
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RunArgs {
    pub name: VaultName,
    /// `None`: the config's value, else 5 minutes (resolved at unlock).
    pub lock_after: Option<u32>,
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

/// A directory path from argv (`--at DIR`, `--add NAME DIR`). Only its
/// syntax is checked here: not empty and not starting with `-`, so an
/// option typed in the wrong place is not taken as a path. Whether it is a
/// safe vault directory is decided when it is opened (#12).
#[derive(PartialEq, Eq)]
pub struct ArgPath(PathBuf);

/// Never shows the path: it came from argv, like `VaultName`.
impl fmt::Debug for ArgPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArgPath(..)")
    }
}

impl ArgPath {
    fn new(path: OsString) -> Option<Self> {
        let first = path.as_bytes().first()?;
        (*first != b'-').then(|| Self(PathBuf::from(path)))
    }
}

/// Fixed messages only; never the offending argument.
#[derive(Debug, PartialEq, Eq)]
pub enum CliError {
    UnknownArgument,
    MissingVault,
    ExtraArgument,
    BadVaultName,
    BadLockAfter,
    BadPath,
    ConflictingModes,
    AtWithoutInit,
    AddNeedsDir,
    LockAfterOnlyForRun,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::UnknownArgument => "unknown argument",
            Self::MissingVault => "missing vault name",
            Self::ExtraArgument => "too many arguments",
            Self::BadVaultName => {
                "vault name must be 1 to 64 letters, digits, - or _, starting with a letter or digit"
            }
            Self::BadLockAfter => "--lock-after needs whole minutes from 1 to 1440",
            Self::BadPath => "a directory must be a non-empty path that does not start with -",
            Self::ConflictingModes => "use only one of --init, --add and --remove",
            Self::AtWithoutInit => "--at works only with --init",
            Self::AddNeedsDir => "--add needs a vault name and a directory",
            Self::LockAfterOnlyForRun => "--lock-after works only when opening a vault",
        };
        write!(f, "{msg}\n{USAGE}")
    }
}

impl std::error::Error for CliError {}

/// Which command the mode flags selected; plain `crystal NAME` is `Run`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Run,
    Init,
    Add,
    Remove,
}

/// Parses the arguments after the program name. Options and positional
/// arguments may come in any order; which combinations are allowed is
/// checked once at the end. lexopt's own errors are discarded because
/// they quote the argument.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let mut parser = Parser::from_args(args);
    let mut mode = Mode::Run;
    let mut lock_after = None;
    let mut at = None;
    // At most two: NAME, and DIR for --add.
    let mut positionals = Vec::with_capacity(2);
    #[cfg(debug_assertions)]
    let mut crash_for_test = None;

    while let Some(arg) = parser.next().map_err(|_| CliError::UnknownArgument)? {
        match arg {
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help),
            Arg::Long("init") => set_mode(&mut mode, Mode::Init)?,
            Arg::Long("add") => set_mode(&mut mode, Mode::Add)?,
            Arg::Long("remove") => set_mode(&mut mode, Mode::Remove)?,
            Arg::Long("at") => {
                // value() takes the next argument even if it starts with
                // `-`; ArgPath::new refuses that.
                let dir = parser.value().ok().and_then(ArgPath::new);
                if at.replace(dir.ok_or(CliError::BadPath)?).is_some() {
                    return Err(CliError::ExtraArgument);
                }
            }
            Arg::Long("lock-after") => {
                let minutes = parser
                    .value()
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                    .filter(|m| (1..=MAX_LOCK_AFTER_MINUTES).contains(m))
                    .ok_or(CliError::BadLockAfter)?;
                lock_after = Some(minutes);
            }
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
            Arg::Value(value) if positionals.len() < 2 => positionals.push(value),
            Arg::Value(_) => return Err(CliError::ExtraArgument),
            _ => return Err(CliError::UnknownArgument),
        }
    }

    if at.is_some() && mode != Mode::Init {
        return Err(CliError::AtWithoutInit);
    }
    if lock_after.is_some() && mode != Mode::Run {
        return Err(CliError::LockAfterOnlyForRun);
    }
    #[cfg(debug_assertions)]
    if crash_for_test.is_some() && mode != Mode::Run {
        return Err(CliError::UnknownArgument);
    }

    let mut positionals = positionals.into_iter();
    let name = positionals.next().ok_or(CliError::MissingVault)?;
    let name = VaultName::new(name).ok_or(CliError::BadVaultName)?;
    let second = positionals.next();
    match mode {
        Mode::Add => {
            let dir = second.ok_or(CliError::AddNeedsDir)?;
            let dir = ArgPath::new(dir).ok_or(CliError::BadPath)?;
            Ok(Command::Add { name, dir })
        }
        _ if second.is_some() => Err(CliError::ExtraArgument),
        Mode::Run => Ok(Command::Run(RunArgs {
            name,
            lock_after,
            #[cfg(debug_assertions)]
            crash_for_test,
        })),
        Mode::Init => Ok(Command::Init { name, at }),
        Mode::Remove => Ok(Command::Remove { name }),
    }
}

/// Selects a mode once; a second mode flag (even the same one) is refused.
fn set_mode(mode: &mut Mode, new: Mode) -> Result<(), CliError> {
    if *mode != Mode::Run {
        return Err(CliError::ConflictingModes);
    }
    *mode = new;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, CliError> {
        parse(args.iter().map(OsString::from))
    }

    fn name(s: &str) -> VaultName {
        VaultName::new(OsString::from(s)).unwrap()
    }

    fn path(s: &str) -> ArgPath {
        ArgPath::new(OsString::from(s)).unwrap()
    }

    fn run(vault_name: &str, lock_after: Option<u32>) -> Command {
        Command::Run(RunArgs {
            name: name(vault_name),
            lock_after,
            #[cfg(debug_assertions)]
            crash_for_test: None,
        })
    }

    #[test]
    fn run_defaults() {
        assert_eq!(parse_strs(&["v"]), Ok(run("v", None)));
    }

    #[test]
    fn run_options_in_any_order() {
        let want = Ok(run("v", Some(30)));
        assert_eq!(parse_strs(&["--lock-after", "30", "v"]), want);
        assert_eq!(parse_strs(&["v", "--lock-after=30"]), want);
    }

    #[test]
    fn init_with_and_without_at() {
        let plain = Ok(Command::Init {
            name: name("v"),
            at: None,
        });
        assert_eq!(parse_strs(&["--init", "v"]), plain);
        assert_eq!(parse_strs(&["v", "--init"]), plain);

        let at = Ok(Command::Init {
            name: name("v"),
            at: Some(path("/sync/v")),
        });
        assert_eq!(parse_strs(&["--init", "v", "--at", "/sync/v"]), at);
        assert_eq!(parse_strs(&["--at=/sync/v", "v", "--init"]), at);
    }

    #[test]
    fn add_and_remove() {
        let add = Ok(Command::Add {
            name: name("v"),
            dir: path("/sync/v"),
        });
        assert_eq!(parse_strs(&["--add", "v", "/sync/v"]), add);
        assert_eq!(parse_strs(&["v", "/sync/v", "--add"]), add);
        assert_eq!(
            parse_strs(&["--remove", "v"]),
            Ok(Command::Remove { name: name("v") })
        );
    }

    #[test]
    fn help() {
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h", "v"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["--init", "v", "-h"]), Ok(Command::Help));
    }

    #[test]
    fn vault_name_is_required_unique_and_valid() {
        assert_eq!(parse_strs(&[]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["--init"]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["--remove"]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["a", "b"]), Err(CliError::ExtraArgument));
        assert_eq!(parse_strs(&["a", "b", "c"]), Err(CliError::ExtraArgument));
        assert_eq!(parse_strs(&["../x"]), Err(CliError::BadVaultName));
        assert_eq!(
            parse_strs(&["--add", "../x", "/d"]),
            Err(CliError::BadVaultName)
        );
    }

    #[test]
    fn at_needs_init() {
        assert_eq!(
            parse_strs(&["--at", "/d", "v"]),
            Err(CliError::AtWithoutInit)
        );
        assert_eq!(
            parse_strs(&["--add", "v", "/d", "--at", "/e"]),
            Err(CliError::AtWithoutInit)
        );
        assert_eq!(
            parse_strs(&["--init", "v", "--at", "/d", "--at", "/e"]),
            Err(CliError::ExtraArgument)
        );
    }

    #[test]
    fn add_needs_name_and_dir() {
        assert_eq!(parse_strs(&["--add", "v"]), Err(CliError::AddNeedsDir));
        assert_eq!(
            parse_strs(&["--add", "v", "/d", "/e"]),
            Err(CliError::ExtraArgument)
        );
    }

    #[test]
    fn modes_cannot_be_combined() {
        for args in [
            &["--remove", "v", "--init"][..],
            &["--init", "--add", "v", "/d"],
            &["--add", "--remove", "v"],
            &["--init", "--init", "v"],
        ] {
            assert_eq!(
                parse_strs(args),
                Err(CliError::ConflictingModes),
                "{args:?}"
            );
        }
    }

    #[test]
    fn only_run_takes_one_name() {
        assert_eq!(
            parse_strs(&["--init", "v", "/d"]),
            Err(CliError::ExtraArgument)
        );
        assert_eq!(
            parse_strs(&["--remove", "v", "/d"]),
            Err(CliError::ExtraArgument)
        );
    }

    #[test]
    fn lock_after_only_when_opening() {
        for args in [
            &["--init", "v", "--lock-after", "5"][..],
            &["--add", "v", "/d", "--lock-after", "5"],
            &["--remove", "v", "--lock-after", "5"],
        ] {
            assert_eq!(
                parse_strs(args),
                Err(CliError::LockAfterOnlyForRun),
                "{args:?}"
            );
        }
    }

    // A path that looks like an option is refused, also where lexopt would
    // hand it over as a plain value (after `--at`, after `--`).
    #[test]
    fn paths_must_not_look_like_options() {
        for args in [
            &["--init", "v", "--at", "-d"][..],
            &["--init", "v", "--at=-d"],
            &["--init", "v", "--at", ""],
            &["--init", "v", "--at"],
            &["--add", "v", "-"],
            &["--add", "v", "--", "-d"],
            &["--add", "v", ""],
        ] {
            assert_eq!(parse_strs(args), Err(CliError::BadPath), "{args:?}");
        }
        // Without `--`, lexopt sees an option and it is unknown.
        assert_eq!(
            parse_strs(&["--add", "v", "-d"]),
            Err(CliError::UnknownArgument)
        );
    }

    #[test]
    fn lock_after_bounds() {
        let lock_after = |m: &str| parse_strs(&["--lock-after", m, "v"]);
        assert_eq!(lock_after("1"), Ok(run("v", Some(1))));
        assert_eq!(lock_after("1440"), Ok(run("v", Some(1440))));
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
        let attempts: [&[&str]; 14] = [
            &["--passphrase=hunter2", "v"],
            &["--passphrase", "hunter2", "v"],
            &["--init=hunter2", "v"],
            &["--lock-after", "hunter2", "v"],
            &["v", "hunter2", "x"],
            &["hunter2/../x"],
            &["--at", "/hunter2", "v"],
            &["--init", "v", "--at", "-hunter2"],
            &["--init", "v", "hunter2"],
            &["--add", "hunter2"],
            &["--add", "v", "--", "-hunter2"],
            &["--add", "hunter2/x", "/d"],
            &["--remove", "v", "hunter2"],
            &["--remove", "hunter2", "--init"],
        ];
        for args in attempts {
            let err = parse_strs(args).unwrap_err();
            assert!(!err.to_string().contains("hunter2"), "{args:?}");
            assert!(!format!("{err:?}").contains("hunter2"), "{args:?}");
        }
    }

    // A passphrase typed in place of a name or path can pass validation;
    // the parsed arguments must still never show it.
    #[test]
    fn parsed_args_never_show_names_or_paths() {
        for args in [
            &["hunter2"][..],
            &["--init", "hunter2", "--at", "/hunter2"],
            &["--add", "hunter2", "hunter2"],
            &["--remove", "hunter2"],
        ] {
            let parsed = parse_strs(args).unwrap();
            assert!(!format!("{parsed:?}").contains("hunter2"), "{parsed:?}");
        }
    }
}
