//! Command-line arguments (startup step 6; decision 0032). Never carries
//! secrets: argv is readable by other processes through /proc/<pid>/cmdline.
//! Errors never echo argument text, since a mistyped argument could be a
//! passphrase.

use lexopt::{Arg, Parser};
#[cfg(debug_assertions)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStrExt;

use crate::vault::VaultName;

pub const USAGE: &str = "\
usage: crystal unlock NAME        open the vault NAME
       crystal init NAME          create a vault
       crystal recover NAME       set a new passphrase with the recovery code
       crystal check-code NAME    check the recovery code; changes nothing
       crystal rm NAME            delete a vault for good
       crystal help
Vaults live in ~/.local/share/crystal.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `unlock NAME`: open a vault.
    Run(RunArgs),
    /// `init NAME`: create a vault.
    Init {
        name: VaultName,
    },
    /// `recover NAME`: a new passphrase, with the recovery code.
    Recover {
        name: VaultName,
    },
    /// `check-code NAME`: check the recovery code; writes nothing.
    CheckCode {
        name: VaultName,
    },
    /// `rm NAME`: delete a vault for good.
    Rm {
        name: VaultName,
    },
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RunArgs {
    pub name: VaultName,
    /// Debug builds only: crash after entering the alternate screen, for
    /// the tests of abrupt exits. Not compiled into release builds.
    #[cfg(debug_assertions)]
    pub crash_for_test: Option<CrashKind>,
    /// Debug builds only: run the passphrase prompt on the alternate
    /// screen and print only the resulting length. Not in release builds.
    #[cfg(debug_assertions)]
    pub prompt_for_test: Option<PromptKind>,
    /// Debug builds only: skip the sandbox and unlock (steps 8 to 11) and wait for a key at once,
    /// for the tests of steps 1 to 7, signals and the terminal guard,
    /// which need no vault. Not in release builds.
    #[cfg(debug_assertions)]
    pub skip_unlock_for_test: bool,
    /// Debug builds only: after unlock, add a few notes in memory, so the
    /// shell, pager and clipboard can be tested through the binary on
    /// fixed notes, a hostile one among them. They are never saved: the
    /// session refuses every save. Not in release builds.
    #[cfg(debug_assertions)]
    pub demo_notes_for_test: bool,
    /// Debug builds only: auto-lock after this many seconds instead of 5
    /// minutes, so a pty test sees it (decision 0048).
    #[cfg(debug_assertions)]
    pub lock_after_for_test: Option<u64>,
    /// Debug builds only: enter the sandbox, try the one forbidden action
    /// `--sandbox-check-for-test CHECK` names, print the outcome and exit
    /// (docs/design/sandboxing.md, section 9). Not in release builds.
    #[cfg(debug_assertions)]
    pub sandbox_check_for_test: Option<SandboxCheck>,
}

/// Which prompt `--prompt-for-test KIND` runs. Debug builds only.
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `read`: one entry, as at unlock.
    Read,
    /// `confirm`: two entries that must match, as for `crystal init`.
    Confirm,
}

/// What `--sandbox-check-for-test CHECK` tries inside the sandbox. One
/// check per process: a seccomp kill ends it. Debug builds only.
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxCheck {
    /// `read-outside`: read `$HOME/.ssh/id_ed25519`.
    ReadOutside,
    /// `write-outside`: create `$HOME/outside` (not beneath the vault or
    /// the state directory).
    WriteOutside,
    /// `other-vault`: read another vault's `VERSION`.
    OtherVault,
    /// `wait`: do nothing, wait for a key or a signal, so the test can
    /// read `/proc/PID/status` from outside.
    Wait,
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
    MissingCommand,
    MissingVault,
    ExtraArgument,
    BadVaultName,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::UnknownArgument => "unknown argument",
            Self::MissingCommand => {
                "the first word must be a command: unlock, init, recover, check-code, rm or help"
            }
            Self::MissingVault => "missing vault name",
            Self::ExtraArgument => "too many arguments",
            Self::BadVaultName => {
                "vault name must be 1 to 64 letters, digits, - or _, starting with a letter or \
                 digit, and not init, rm, recover, check-code or help"
            }
        };
        write!(f, "{msg}\n{USAGE}")
    }
}

impl std::error::Error for CliError {}

/// Which command the first word selected. There is no default: a bare
/// `crystal NAME` is refused (decision 0058).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Unlock,
    Init,
    Recover,
    CheckCode,
    Rm,
}

/// Parses the arguments after the program name: a command word, then
/// exactly one vault name. lexopt's own errors are discarded because
/// they quote the argument.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let mut parser = Parser::from_args(args);
    let mut mode = None;
    let mut name = None;
    #[cfg(debug_assertions)]
    let mut crash_for_test = None;
    #[cfg(debug_assertions)]
    let mut prompt_for_test = None;
    #[cfg(debug_assertions)]
    let mut skip_unlock_for_test = false;
    #[cfg(debug_assertions)]
    let mut demo_notes_for_test = false;
    #[cfg(debug_assertions)]
    let mut lock_after_for_test = None;
    #[cfg(debug_assertions)]
    let mut sandbox_check_for_test = None;

    while let Some(arg) = parser.next().map_err(|_| CliError::UnknownArgument)? {
        match arg {
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help),
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
            #[cfg(debug_assertions)]
            Arg::Long("prompt-for-test") => {
                let kind = match parser.value().ok().as_deref().and_then(OsStr::to_str) {
                    Some("read") => PromptKind::Read,
                    Some("confirm") => PromptKind::Confirm,
                    _ => return Err(CliError::UnknownArgument),
                };
                prompt_for_test = Some(kind);
            }
            #[cfg(debug_assertions)]
            Arg::Long("skip-unlock-for-test") => skip_unlock_for_test = true,
            #[cfg(debug_assertions)]
            Arg::Long("demo-notes-for-test") => demo_notes_for_test = true,
            #[cfg(debug_assertions)]
            Arg::Long("lock-after-for-test") => {
                let value = parser.value().ok();
                let seconds = value.as_deref().and_then(OsStr::to_str);
                let seconds = seconds.and_then(|s| s.parse().ok());
                lock_after_for_test = Some(seconds.ok_or(CliError::UnknownArgument)?);
            }
            #[cfg(debug_assertions)]
            Arg::Long("sandbox-check-for-test") => {
                let check = match parser.value().ok().as_deref().and_then(OsStr::to_str) {
                    Some("read-outside") => SandboxCheck::ReadOutside,
                    Some("write-outside") => SandboxCheck::WriteOutside,
                    Some("other-vault") => SandboxCheck::OtherVault,
                    Some("wait") => SandboxCheck::Wait,
                    _ => return Err(CliError::UnknownArgument),
                };
                sandbox_check_for_test = Some(check);
            }
            // The first word is always the command; anything else there,
            // such as a bare vault name, is refused without echoing it.
            Arg::Value(value) if mode.is_none() => match value.as_bytes() {
                b"unlock" => mode = Some(Mode::Unlock),
                b"init" => mode = Some(Mode::Init),
                b"recover" => mode = Some(Mode::Recover),
                b"check-code" => mode = Some(Mode::CheckCode),
                b"rm" => mode = Some(Mode::Rm),
                b"help" => return Ok(Command::Help),
                _ => return Err(CliError::MissingCommand),
            },
            Arg::Value(value) if name.is_none() => name = Some(value),
            Arg::Value(_) => return Err(CliError::ExtraArgument),
            _ => return Err(CliError::UnknownArgument),
        }
    }

    let mode = mode.ok_or(CliError::MissingCommand)?;

    #[cfg(debug_assertions)]
    if (crash_for_test.is_some()
        || prompt_for_test.is_some()
        || skip_unlock_for_test
        || demo_notes_for_test
        || lock_after_for_test.is_some()
        || sandbox_check_for_test.is_some())
        && mode != Mode::Unlock
    {
        return Err(CliError::UnknownArgument);
    }

    let name = name.ok_or(CliError::MissingVault)?;
    let name = VaultName::new(name).ok_or(CliError::BadVaultName)?;
    Ok(match mode {
        Mode::Unlock => Command::Run(RunArgs {
            name,
            #[cfg(debug_assertions)]
            crash_for_test,
            #[cfg(debug_assertions)]
            prompt_for_test,
            #[cfg(debug_assertions)]
            skip_unlock_for_test,
            #[cfg(debug_assertions)]
            demo_notes_for_test,
            #[cfg(debug_assertions)]
            lock_after_for_test,
            #[cfg(debug_assertions)]
            sandbox_check_for_test,
        }),
        Mode::Init => Command::Init { name },
        Mode::Recover => Command::Recover { name },
        Mode::CheckCode => Command::CheckCode { name },
        Mode::Rm => Command::Rm { name },
    })
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

    fn run(vault_name: &str) -> Command {
        Command::Run(RunArgs {
            name: name(vault_name),
            #[cfg(debug_assertions)]
            crash_for_test: None,
            #[cfg(debug_assertions)]
            prompt_for_test: None,
            #[cfg(debug_assertions)]
            skip_unlock_for_test: false,
            #[cfg(debug_assertions)]
            demo_notes_for_test: false,
            #[cfg(debug_assertions)]
            lock_after_for_test: None,
            #[cfg(debug_assertions)]
            sandbox_check_for_test: None,
        })
    }

    #[test]
    fn commands() {
        assert_eq!(parse_strs(&["unlock", "v"]), Ok(run("v")));
        assert_eq!(
            parse_strs(&["init", "v"]),
            Ok(Command::Init { name: name("v") })
        );
        assert_eq!(
            parse_strs(&["recover", "v"]),
            Ok(Command::Recover { name: name("v") })
        );
        assert_eq!(
            parse_strs(&["check-code", "v"]),
            Ok(Command::CheckCode { name: name("v") })
        );
        assert_eq!(
            parse_strs(&["rm", "v"]),
            Ok(Command::Rm { name: name("v") })
        );
    }

    #[test]
    fn help() {
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h", "v"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["init", "v", "-h"]), Ok(Command::Help));
    }

    // Removed commands and options are refused, not silently accepted;
    // that includes the bare `crystal NAME` (decision 0058).
    #[test]
    fn removed_commands_and_options_are_refused() {
        assert_eq!(parse_strs(&["v"]), Err(CliError::MissingCommand));
        assert_eq!(parse_strs(&["add", "v"]), Err(CliError::MissingCommand));
        assert_eq!(parse_strs(&["remove", "v"]), Err(CliError::MissingCommand));
        for args in [
            &["--init", "v"][..],
            &["init", "v", "--at", "/d"],
            &["--lock-after", "5", "v"],
        ] {
            assert_eq!(parse_strs(args), Err(CliError::UnknownArgument), "{args:?}");
        }
        assert_eq!(
            parse_strs(&["recover", "v", "/d"]),
            Err(CliError::ExtraArgument)
        );
    }

    // The auto-lock test hook (debug builds): seconds, with `crystal
    // unlock NAME` only.
    #[cfg(debug_assertions)]
    #[test]
    fn lock_after_for_test() {
        let Ok(Command::Run(args)) = parse_strs(&["--lock-after-for-test", "2", "unlock", "v"])
        else {
            panic!("not run");
        };
        assert_eq!(args.lock_after_for_test, Some(2));
        for bad in [
            &["--lock-after-for-test", "x", "unlock", "v"][..],
            &["--lock-after-for-test", "-1", "unlock", "v"],
            &["init", "v", "--lock-after-for-test", "2"],
        ] {
            assert_eq!(parse_strs(bad), Err(CliError::UnknownArgument), "{bad:?}");
        }
    }

    // Release builds: every debug-only test hook is an unknown argument,
    // given exactly as a debug build accepts it, so only a missing flag
    // can refuse it. Auto-lock stays fixed at 5 minutes (decision 0048).
    // CI runs this with `cargo test --release`.
    #[cfg(not(debug_assertions))]
    #[test]
    fn release_refuses_the_test_hooks() {
        assert!(matches!(parse_strs(&["unlock", "v"]), Ok(Command::Run(_))));
        for hook in [
            &["--crash-for-test", "panic", "unlock", "v"][..],
            &["--prompt-for-test", "read", "unlock", "v"],
            &["--skip-unlock-for-test", "unlock", "v"],
            &["--demo-notes-for-test", "unlock", "v"],
            &["--lock-after-for-test", "2", "unlock", "v"],
            &["--lock-after", "2", "unlock", "v"],
            &["--sandbox-check-for-test", "wait", "unlock", "v"],
        ] {
            assert_eq!(parse_strs(hook), Err(CliError::UnknownArgument), "{hook:?}");
        }
    }

    // Command words are reserved: never a vault name, and a command word
    // after the first word is a (refused) name, not a second command.
    #[test]
    fn command_words_are_not_vault_names() {
        for word in ["init", "rm", "recover", "check-code", "help"] {
            assert!(VaultName::new(OsString::from(word)).is_none(), "{word}");
        }
        assert_eq!(parse_strs(&["init", "init"]), Err(CliError::BadVaultName));
        assert_eq!(
            parse_strs(&["rm", "check-code"]),
            Err(CliError::BadVaultName)
        );
        assert_eq!(parse_strs(&["v", "init"]), Err(CliError::MissingCommand));
        assert_eq!(
            parse_strs(&["unlock", "v", "init"]),
            Err(CliError::ExtraArgument)
        );
        // Names that only start with a command word are fine; `add`,
        // `remove` and `unlock` are ordinary names.
        assert_eq!(parse_strs(&["unlock", "init2"]), Ok(run("init2")));
        assert_eq!(parse_strs(&["unlock", "Init"]), Ok(run("Init")));
        assert_eq!(parse_strs(&["unlock", "remove"]), Ok(run("remove")));
        assert_eq!(parse_strs(&["unlock", "unlock"]), Ok(run("unlock")));
    }

    #[test]
    fn vault_name_is_required_unique_and_valid() {
        assert_eq!(parse_strs(&[]), Err(CliError::MissingCommand));
        assert_eq!(parse_strs(&["unlock"]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["init"]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["rm"]), Err(CliError::MissingVault));
        assert_eq!(parse_strs(&["a", "b"]), Err(CliError::MissingCommand));
        assert_eq!(
            parse_strs(&["unlock", "a", "b"]),
            Err(CliError::ExtraArgument)
        );
        assert_eq!(parse_strs(&["rm", "a", "b"]), Err(CliError::ExtraArgument));
        assert_eq!(parse_strs(&["unlock", "../x"]), Err(CliError::BadVaultName));
        assert_eq!(parse_strs(&["init", "a/b"]), Err(CliError::BadVaultName));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_hooks_only_when_opening() {
        assert_eq!(
            parse_strs(&["init", "v", "--skip-unlock-for-test"]),
            Err(CliError::UnknownArgument)
        );
    }

    // A mistyped secret must never be echoed back to the terminal.
    #[test]
    fn errors_never_echo_arguments() {
        let attempts: [&[&str]; 9] = [
            &["hunter2"],
            &["--passphrase=hunter2", "v"],
            &["--passphrase", "hunter2", "v"],
            &["init=hunter2", "v"],
            &["v", "hunter2", "x"],
            &["hunter2/../x"],
            &["init", "v", "hunter2"],
            &["rm", "hunter2/x"],
            &["recover", "v", "hunter2"],
        ];
        for args in attempts {
            let err = parse_strs(args).unwrap_err();
            assert!(!err.to_string().contains("hunter2"), "{args:?}");
            assert!(!format!("{err:?}").contains("hunter2"), "{args:?}");
        }
    }

    // A passphrase typed in place of a name can pass validation; the
    // parsed arguments must still never show it.
    #[test]
    fn parsed_args_never_show_names() {
        for args in [
            &["unlock", "hunter2"][..],
            &["init", "hunter2"],
            &["rm", "hunter2"],
            &["check-code", "hunter2"],
        ] {
            let parsed = format!("{:?}", parse_strs(args).unwrap());
            assert!(!parsed.contains("hunter2"), "{args:?}: {parsed}");
        }
    }
}
