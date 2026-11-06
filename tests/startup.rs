//! Child-process tests of the real binary's startup order (docs/design/startup.md).
//!
//! They need RLIMIT_MEMLOCK at the floor (docs/guide/setup.md; CI raises it).
//! Below it the binary stops at step 4 and these fail with the memlock
//! message instead.
//!
//! Every run gets its own HOME under cargo's test directory, so the tests
//! never create or touch the real ~/.config/crystal or
//! ~/.local/state/crystal.

use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_crystal");

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A fresh, empty HOME for one test.
fn fresh_home(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Creates HOME/.config/crystal, both levels 0700, as docs/guide/setup.md tells
/// users to until --init exists. Until unlock exists (#15), a run with it
/// reaches the terminal guard for any valid vault name.
fn make_config(home: &Path) {
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("crystal")).unwrap();
    set_mode(&config, 0o700);
    set_mode(&config.join("crystal"), 0o700);
}

/// Runs crystal with `args` (shell words) in a new pseudo-terminal, with
/// HOME set to `home` or unset, and types `q` so its key wait ends.
/// Returns (success, everything it printed).
fn run_in_pty(args: &str, home: Option<&Path>) -> (bool, String) {
    run_shell_in_pty(&format!("'{BIN}' {args}"), home)
}

/// Runs a whole shell command line in a new pseudo-terminal and types `q`.
fn run_shell_in_pty(cmd: &str, home: Option<&Path>) -> (bool, String) {
    pty(cmd, home, b"q")
}

/// `script -e` returns the command's exit code; the command's stdout and
/// stderr both arrive on script's stdout. `keys` are typed into the
/// terminal, then script's stdin is held open until it exits: closing it
/// would make script send end-of-file, which Crystal reads as a key.
/// The command runs in `home` when it is a real directory, so a core file
/// would land where tests look.
fn pty(cmd: &str, home: Option<&Path>, keys: &[u8]) -> (bool, String) {
    let mut script = Command::new("script");
    script
        .args(["-qec", cmd, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    match home {
        Some(home) => script.env("HOME", home),
        None => script.env_remove("HOME"),
    };
    if let Some(home) = home.filter(|h| h.is_dir()) {
        script.current_dir(home);
    }
    let mut child = script.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(keys).unwrap();
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    drop(stdin);
    (child.wait().unwrap().success(), text(&output))
}

/// Step 5: with no controlling terminal, Crystal refuses to start.
#[test]
fn refuses_without_terminal() {
    let home = fresh_home("no_terminal");
    make_config(&home);
    // New session: no controlling terminal, even when `cargo test` itself
    // runs in an interactive shell.
    let out = Command::new("setsid")
        .args(["--wait", BIN, "diary"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("cannot open /dev/tty"),
        "{}",
        text(&out.stderr)
    );
}

/// Step 5: inside a terminal, Crystal starts even with stdin and stdout
/// redirected, because it reads and writes /dev/tty directly.
#[test]
fn starts_in_terminal_with_redirected_stdio() {
    let home = fresh_home("redirected_stdio");
    make_config(&home);
    let (ok, output) = run_in_pty("diary </dev/null >/dev/null", Some(&home));
    assert!(ok, "{output}");
}

/// Step 6: a secret on the command line is refused and never echoed.
#[test]
fn rejects_secret_argument_without_echoing_it() {
    let home = fresh_home("secret_argument");
    make_config(&home);
    let (ok, output) = run_in_pty("--passphrase hunter2 diary", Some(&home));
    assert!(!ok, "{output}");
    assert!(output.contains("unknown argument"), "{output}");
    assert!(!output.contains("hunter2"), "{output}");
}

/// Step 6: a vault name that could escape ~/Vaults is refused, not echoed,
/// and nothing is created.
#[test]
fn rejects_unsafe_vault_name() {
    let home = fresh_home("unsafe_name");
    make_config(&home);
    for name in ["../hunter2", "hunter2/x", ".hunter2"] {
        let (ok, output) = run_in_pty(&format!("'{name}'"), Some(&home));
        assert!(!ok && output.contains("vault name must be"), "{output}");
        assert!(!output.contains("hunter2"), "{output}");
    }
    assert!(!home.join(".local").exists());
}

/// Step 2: an fd inherited from whatever started Crystal would survive
/// Landlock and seccomp, so Crystal refuses to start with one open.
#[test]
fn refuses_inherited_file_descriptor() {
    let home = fresh_home("inherited_fd");
    make_config(&home);
    let (ok, output) = run_in_pty("diary 5</dev/null", Some(&home));
    assert!(
        !ok && output.contains("1 open file descriptor(s) inherited")
            && output.contains("lowest 5"),
        "{output}"
    );
    // Nothing opened or created before the refusal.
    assert!(!home.join(".local").exists());
}

/// Phase 1 acceptance, checked on the real running binary rather than on
/// the test process: non-dumpable (the kernel then makes /proc/<pid> owned
/// by root, so same-user processes cannot read its memory or environment;
/// on hosts with SELinux deny_ptrace, `gdb -p` fails for everything and so
/// proves nothing), core dumps off with the hard limit too, memory locked,
/// no new privileges, umask 077. Unlike a missing core file, these fail on
/// any host where the hardening is missing, whatever its core_pattern.
#[test]
fn running_binary_is_hardened() {
    let home = fresh_home("hardened");
    make_config(&home);
    let cmd = format!(
        "'{BIN}' diary & p=$!; sleep 1; echo; \
         echo environ_owner=$(stat -c %u /proc/$p/environ); \
         grep -E '^(VmLck|NoNewPrivs|Umask):' /proc/$p/status; \
         grep 'Max core file size' /proc/$p/limits; \
         kill -TERM $p; wait $p; echo exit=$?"
    );
    let (ok, output) = pty(&cmd, Some(&home), b"");
    assert!(ok, "{output}");
    let field = |name: &str| -> Vec<String> {
        output
            .lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("no {name}: {output:?}"))
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };
    assert!(output.contains("environ_owner=0"), "dumpable: {output:?}");
    let locked_kib: u64 = field("VmLck:")[1].parse().unwrap();
    assert!(locked_kib > 0, "{output:?}");
    assert_eq!(field("NoNewPrivs:")[1], "1", "{output:?}");
    assert_eq!(field("Umask:")[1], "0077", "{output:?}");
    // "Max core file size  0  0  bytes": soft and hard limit.
    assert_eq!(field("Max core file size")[4..6], ["0", "0"], "{output:?}");
    assert!(output.contains("exit=0"), "{output:?}");
}

/// Step 7: a normal run needs ~/.config/crystal and never creates it, nor
/// anything else, and never looks at ~/Vaults. It must be a real directory,
/// ours, mode 0700, reached without a symlink. (Owned by another user needs
/// root to set up; the vault unit test foreign_owned_directories_are_refused
/// covers that check.)
#[test]
fn config_dir_checks() {
    let home = fresh_home("config_dir");
    let config = home.join(".config/crystal");

    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("~/.config/crystal does not exist"),
        "{output}"
    );
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "a run without a config must create nothing"
    );

    make_config(&home);
    set_mode(&config, 0o755);
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("~/.config/crystal has mode 755, need 700"),
        "{output}"
    );
    assert_eq!(mode(&config), 0o755, "Crystal must not change the mode");

    // A symlink in place of ~/.config/crystal.
    let elsewhere = home.join("elsewhere");
    std::fs::rename(&config, &elsewhere).unwrap();
    set_mode(&elsewhere, 0o700);
    symlink(&elsewhere, &config).unwrap();
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("~/.config/crystal is a symlink or not a directory"),
        "{output}"
    );

    // A symlink in place of ~/.config, pointing at a valid layout.
    let real_config = home.join("real-config");
    std::fs::rename(home.join(".config"), &real_config).unwrap();
    std::fs::remove_file(real_config.join("crystal")).unwrap();
    std::fs::rename(&elsewhere, real_config.join("crystal")).unwrap();
    symlink(&real_config, home.join(".config")).unwrap();
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("a parent of ~/.config/crystal is a symlink"),
        "{output}"
    );
    // Refused before the state directory was touched.
    assert!(!home.join(".local").exists());
}

/// Step 7: a fresh state directory is created with every level 0700.
#[test]
fn creates_state_dir_with_mode_0700() {
    let home = fresh_home("state_dir_fresh");
    make_config(&home);
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(ok, "{output}");
    for dir in [".local", ".local/state", ".local/state/crystal"] {
        assert_eq!(mode(&home.join(dir)), 0o700, "{dir}");
    }
    // A second start accepts the directory it created.
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(ok, "{output}");
}

/// Step 7: an existing state directory with a looser mode is refused,
/// not silently fixed.
#[test]
fn refuses_state_dir_with_loose_mode() {
    let home = fresh_home("state_dir_loose");
    make_config(&home);
    let state = home.join(".local/state/crystal");
    std::fs::create_dir_all(&state).unwrap();
    set_mode(&state, 0o755);
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(!ok && output.contains("mode 755, need 700"), "{output}");
    assert!(output.contains("chmod 700"), "{output}");
    assert_eq!(
        mode(&state),
        0o755,
        "Crystal must not change the mode itself"
    );
}

/// Step 7: a symlink in place of the state directory is refused.
#[test]
fn refuses_state_dir_symlink() {
    let home = fresh_home("state_dir_symlink");
    make_config(&home);
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(home.join(".local/state")).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    set_mode(&elsewhere, 0o700);
    symlink(&elsewhere, home.join(".local/state/crystal")).unwrap();
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("is a symlink or not a directory"),
        "{output}"
    );
}

/// Step 7: a symlink in a parent of the state directory is refused too, so
/// nothing can redirect where the generation counter is kept. Nothing is
/// created at the symlink's target.
#[test]
fn refuses_state_dir_parent_symlink() {
    let home = fresh_home("state_parent_symlink");
    make_config(&home);
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(home.join(".local")).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    set_mode(&elsewhere, 0o700);
    symlink(&elsewhere, home.join(".local/state")).unwrap();
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("a parent of ~/.local/state/crystal is a symlink"),
        "{output}"
    );
    assert!(!elsewhere.join("crystal").exists());
}

/// Steps 3 and 7: HOME must be set and absolute.
#[test]
fn refuses_missing_or_relative_home() {
    for home in [None, Some(Path::new("relative/home"))] {
        let (ok, output) = run_in_pty("diary", home);
        assert!(
            !ok && output.contains("HOME is unset or not an absolute path"),
            "{output}"
        );
    }
}

/// Until #13 and #14, --init, --add and --remove say so plainly and create
/// nothing: no config directory, no ~/Vaults, no state directory.
#[test]
fn config_commands_are_not_implemented_yet() {
    let home = fresh_home("config_commands");
    for (args, flag) in [
        ("--init newvault", "--init"),
        ("--init newvault --at /tmp/x", "--init"),
        ("--add newvault /tmp/x", "--add"),
        ("--remove newvault", "--remove"),
    ] {
        let (ok, output) = run_in_pty(args, Some(&home));
        let want = format!("{flag} is not implemented yet");
        assert!(!ok && output.contains(&want), "{args}: {output}");
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "{args}");
    }
}

/// Step 7: the guard enters the alternate screen, clears it before leaving,
/// and restores the exact terminal settings it found.
#[test]
fn terminal_guard_restores_terminal() {
    let home = fresh_home("terminal_guard");
    make_config(&home);
    // echo: Crystal prints no newline, so separate its output from stty's.
    let cmd = format!("stty -g; '{BIN}' diary; echo; stty -g");
    let (ok, output) = run_shell_in_pty(&cmd, Some(&home));
    assert!(ok, "{output}");

    let lines: Vec<&str> = output.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines[0].trim_start_matches('q'), // the typed key, echoed before raw mode
        lines[lines.len() - 1],
        "terminal settings not restored"
    );

    let enter = output
        .find("\x1b[?1049h")
        .expect("never entered the alternate screen");
    let leave = output
        .find("\x1b[2J\x1b[?1049l")
        .expect("never cleared and left it");
    assert!(enter < leave, "{output:?}");
}

/// Phase 1 acceptance: a forced panic leaves a clean, restored terminal,
/// prints only the location, on the normal screen, and never the message.
#[test]
fn forced_panic_leaves_clean_terminal() {
    let home = fresh_home("forced_panic");
    make_config(&home);
    let cmd =
        format!("stty -g; '{BIN}' --crash-for-test panic diary; s=$?; echo; echo exit=$s; stty -g");
    let (ok, output) = run_shell_in_pty(&cmd, Some(&home));
    assert!(ok, "{output}");
    // 101: a Rust panic that unwound out of main, not death by a signal.
    assert!(output.contains("exit=101"), "{output:?}");

    let lines: Vec<&str> = output.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines[0].trim_start_matches('q'), // the typed key, echoed before raw mode
        lines[lines.len() - 1],
        "terminal settings not restored"
    );

    let leave = output
        .find("\x1b[2J\x1b[?1049l")
        .expect("never left the alternate screen");
    let report = output
        .find("crystal: internal error at src/main.rs:")
        .expect("no location report");
    assert!(
        leave < report,
        "report printed on the alternate screen: {output:?}"
    );
    assert!(
        !output.contains("hunter2"),
        "panic message leaked: {output:?}"
    );
}

/// Every file and directory under `dir`, sorted, relative to it.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                todo.push(path.clone());
            }
            found.push(path.strip_prefix(dir).unwrap().to_path_buf());
        }
    }
    found.sort();
    found
}

fn assert_no_core_file(home: &Path, context: &str) {
    let cores: Vec<_> = files_under(home)
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("core"))
        .collect();
    assert!(cores.is_empty(), "{context}: core file left: {cores:?}");
}

/// Abort paths skip Drop, so they cannot zeroize, and a stack overflow
/// cannot even restore the terminal (docs/design/leakage.md lists both as accepted).
/// What every one of them must still do: never print the message, and
/// leave no core file. A panic inside Drop aborts only after the panic
/// hook ran, so it does restore the terminal.
#[test]
fn abort_paths_print_no_secret_and_leave_no_core() {
    // (kind, exit code, terminal restored): 134 = SIGABRT, 139 = SIGSEGV;
    // on musl, Rust's stack guard kills with SIGSEGV and prints nothing.
    for (kind, exit, restored) in [("panic-in-drop", 134, true), ("stack-overflow", 139, false)] {
        let home = fresh_home(&format!("abort_{kind}"));
        make_config(&home);
        let cmd = format!(
            "stty -g; '{BIN}' --crash-for-test {kind} diary; s=$?; echo; echo exit=$s; stty -g"
        );
        let (ok, output) = run_shell_in_pty(&cmd, Some(&home));
        assert!(ok, "{kind}: {output}");
        assert!(
            output.contains(&format!("exit={exit}")),
            "{kind}: {output:?}"
        );
        assert!(
            !output.contains("hunter2"),
            "{kind}: message leaked: {output:?}"
        );
        assert_no_core_file(&home, kind);

        if restored {
            let lines: Vec<&str> = output.split("\r\n").filter(|l| !l.is_empty()).collect();
            assert_eq!(
                lines[0].trim_start_matches('q'),
                lines[lines.len() - 1],
                "{kind}: terminal not restored"
            );
        }
    }
}

/// Closing the terminal window: the pty disappears under Crystal (script
/// is killed, which closes the pty's master side). Crystal must notice the
/// hangup and leave through the normal exit path, so Drop runs. The
/// wrapper shell ignores SIGHUP only to survive and record the exit code;
/// Crystal installs its own handler regardless.
#[test]
fn terminal_hangup_exits_cleanly() {
    let home = fresh_home("hangup");
    make_config(&home);
    let status = home.join("status");
    let cmd = format!(
        "trap '' HUP; '{BIN}' diary; echo $? > '{}'",
        status.display()
    );
    let mut script = Command::new("script")
        .args(["-qec", &cmd, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .env("HOME", &home)
        .current_dir(&home)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    script.kill().unwrap();
    script.wait().unwrap();

    let recorded = (0..50).find_map(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::read_to_string(&status)
            .ok()
            .filter(|s| s.ends_with('\n'))
    });
    assert_eq!(
        recorded.as_deref(),
        Some("0\n"),
        "Crystal did not exit cleanly on hangup"
    );
}

/// kill -9 cannot be caught: no Drop, no restore. It must still leave
/// nothing behind that Crystal wrote; from Phase 4 on this covers temp
/// files during a save. SIGKILL never dumps core.
#[test]
fn kill_9_leaves_no_new_files() {
    let home = fresh_home("kill_9");
    make_config(&home);
    let listing = |name: &str| home.with_extension(name);
    let (before, after) = (listing("before"), listing("after"));
    let cmd = format!(
        "'{BIN}' diary & p=$!; sleep 1; find . | sort > '{}'; kill -9 $p; wait $p; s=$?; \
         find . | sort > '{}'; echo; echo exit=$s",
        before.display(),
        after.display()
    );
    let (ok, output) = pty(&cmd, Some(&home), b"");
    assert!(ok, "{output}");
    assert!(
        output.contains("exit=137"),
        "not killed by SIGKILL: {output:?}"
    );
    let before = std::fs::read_to_string(&before).unwrap();
    assert!(before.contains("./.local/state/crystal"), "{before}");
    assert_eq!(before, std::fs::read_to_string(&after).unwrap());
}

/// Every catchable signal that would kill Crystal (term::EXIT_SIGNALS and
/// the real-time signals) instead ends it through the normal path: exit 0
/// and a restored terminal, not death by signal (128 + signal number) with
/// the terminal left raw. Real-time signals are given by number, since the
/// shell's glibc and Crystal's musl disagree on what RTMIN means; 35 is
/// musl's first free one, 64 the last. No key is typed and stdin stays
/// open, so a clean exit can only come from the signal handler.
#[test]
fn exit_signals_restore_terminal() {
    let signals = [
        "TERM", "HUP", "INT", "QUIT", "USR1", "USR2", "ALRM", "VTALRM", "PROF", "XCPU", "XFSZ",
        "IO", "PWR", "STKFLT", "35", "50", "64",
    ];
    // Each run waits a second for startup; run them side by side.
    std::thread::scope(|scope| {
        for signal in signals {
            scope.spawn(move || exit_signal_restores_terminal(signal));
        }
    });
}

fn exit_signal_restores_terminal(signal: &str) {
    let home = fresh_home(&format!("signal_{signal}"));
    make_config(&home);
    let cmd = format!(
        "stty -g; '{BIN}' diary & p=$!; sleep 1; kill -{signal} $p; wait $p; s=$?; \
         echo; echo exit=$s; stty -g"
    );
    let (ok, output) = pty(&cmd, Some(&home), b"");
    assert!(ok, "{signal}: {output}");
    assert!(output.contains("exit=0"), "{signal}: {output:?}");
    assert!(
        !output.contains("No such process"),
        "{signal}: exited early: {output:?}"
    );

    let lines: Vec<&str> = output.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines[0],
        lines[lines.len() - 1],
        "{signal}: terminal not restored"
    );
    assert!(
        output.contains("\x1b[2J\x1b[?1049l"),
        "{signal}: never left the screen"
    );
}

/// Step 7: SIGTSTP suspends cleanly. While stopped, the process state is
/// T and the terminal is fully restored; SIGCONT re-enters raw mode and the
/// alternate screen; SIGTERM then exits cleanly. No key is typed.
#[test]
fn suspend_restores_terminal_and_resume_reenters() {
    let home = fresh_home("suspend");
    make_config(&home);
    let cmd = format!(
        "stty -g; '{BIN}' diary & p=$!; sleep 1; kill -TSTP $p; sleep 0.5; \
         echo; echo state=$(cut -d' ' -f3 /proc/$p/stat); stty -g; \
         kill -CONT $p; sleep 0.5; kill -TERM $p; wait $p; s=$?; echo; echo exit=$s; stty -g"
    );
    let (ok, output) = pty(&cmd, Some(&home), b"");
    assert!(ok, "{output}");
    assert!(output.contains("state=T"), "not stopped: {output:?}");
    assert!(output.contains("exit=0"), "{output:?}");

    let enter = "\x1b[?1049h\x1b[2J\x1b[H";
    let leave = "\x1b[2J\x1b[?1049l";
    assert_eq!(
        output.matches(enter).count(),
        2,
        "enter, then re-enter after resume"
    );
    assert_eq!(
        output.matches(leave).count(),
        2,
        "leave on suspend and on exit"
    );

    // stty -g lines: before, while stopped, after. All must be identical.
    let plain = output.replace(enter, "").replace(leave, "");
    let settings: Vec<&str> = plain
        .split("\r\n")
        .filter(|l| l.matches(':').count() > 20)
        .collect();
    assert_eq!(settings.len(), 3, "{plain:?}");
    assert!(settings.iter().all(|s| *s == settings[0]), "{settings:?}");
}

/// Runs `cmd` in a new pseudo-terminal, in `home`. For each step, waits
/// until `wait_for` appears in the output (after the previous step's
/// match), then types `keys`; then collects the rest. Waiting matters:
/// keys typed before Crystal enters raw mode would be echoed by the
/// terminal itself. A reader thread lets each wait time out instead of
/// hanging the test run.
fn pty_dialog(cmd: &str, home: &Path, steps: &[(&str, &[u8])]) -> (bool, String) {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Kills `script` if the test panics, so the pty hangs up and Crystal
    /// exits instead of waiting at a prompt forever.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let mut child = KillOnDrop(
        Command::new("script")
            .args(["-qec", cmd, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .env("HOME", home)
            .current_dir(home)
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let mut stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            if tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let next = |output: &mut Vec<u8>| -> bool {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) => {
                output.extend(chunk);
                true
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => false,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("timed out: {:?}", text(output)),
        }
    };
    let mut output = Vec::new();
    let mut seen = 0;
    for (wait_for, keys) in steps {
        // An empty `wait_for` types at once.
        while !wait_for.is_empty() {
            let found = output[seen..]
                .windows(wait_for.len())
                .position(|w| w == wait_for.as_bytes());
            if let Some(pos) = found {
                seen += pos + wait_for.len();
                break;
            }
            assert!(
                next(&mut output),
                "exited before {wait_for:?}: {:?}",
                text(&output)
            );
        }
        stdin.write_all(keys).unwrap();
        stdin.flush().unwrap();
    }
    while next(&mut output) {}
    drop(stdin);
    (child.0.wait().unwrap().success(), text(&output))
}

const PROMPT: &str = "Passphrase: ";
const REPEAT: &str = "Repeat passphrase: ";

/// One `--prompt-for-test` run in a fresh HOME that reaches the prompt.
fn prompt_run(test: &str, kind: &str, steps: &[(&str, &[u8])]) -> (bool, String) {
    let home = fresh_home(test);
    make_config(&home);
    let cmd = format!("'{BIN}' --prompt-for-test {kind} diary");
    pty_dialog(&cmd, &home, steps)
}

/// Acceptance: the passphrase never appears on screen, not even as
/// asterisks. Backspace and Ctrl+U edit it; only its length is printed
/// (debug hook). The prompt is drawn on the alternate screen.
#[test]
fn prompt_reads_passphrase_without_echo() {
    let keys = b"garbage\x15hunter2xx\x7f\x7f\r";
    let (ok, output) = prompt_run("prompt_read", "read", &[(PROMPT, keys)]);
    assert!(ok, "{output:?}");
    assert!(output.contains("len=7"), "{output:?}");
    for leak in ["hunter2", "garbage", "*"] {
        assert!(!output.contains(leak), "{leak:?} shown: {output:?}");
    }
    let enter = output.find("\x1b[?1049h").unwrap();
    let prompt = output.find(PROMPT).unwrap();
    let leave = output.find("\x1b[2J\x1b[?1049l").unwrap();
    assert!(enter < prompt && prompt < leave, "{output:?}");
}

/// Keys typed before the prompt appeared (here during `sleep`, before
/// Crystal even starts) were echoed by the terminal; they are discarded,
/// not taken as the start of the passphrase.
#[test]
fn prompt_discards_keys_typed_before_it() {
    let home = fresh_home("prompt_typeahead");
    make_config(&home);
    let cmd = format!("sleep 1; '{BIN}' --prompt-for-test read diary");
    let steps: [(&str, &[u8]); 2] = [("", b"early"), (PROMPT, b"pw\r")];
    let (ok, output) = pty_dialog(&cmd, &home, &steps);
    assert!(ok, "{output:?}");
    assert!(output.contains("len=2"), "{output:?}");
}

/// --init's variant: two matching entries are accepted, different ones
/// refused.
#[test]
fn prompt_confirm_requires_matching_entries() {
    let same: [(&str, &[u8]); 2] = [(PROMPT, b"abc\r"), (REPEAT, b"abc\r")];
    let (ok, output) = prompt_run("prompt_confirm", "confirm", &same);
    assert!(ok && output.contains("len=3"), "{output:?}");

    let differ: [(&str, &[u8]); 2] = [(PROMPT, b"abc\r"), (REPEAT, b"abd\r")];
    let (ok, output) = prompt_run("prompt_mismatch", "confirm", &differ);
    assert!(!ok, "{output:?}");
    assert!(
        output.contains("crystal: the passphrases do not match"),
        "{output:?}"
    );
}

/// Ctrl+C and Esc cancel; the error names no typed byte.
#[test]
fn prompt_cancel_prints_nothing_typed() {
    for (test, key) in [("prompt_ctrl_c", b'\x03'), ("prompt_esc", b'\x1b')] {
        let keys = [b"hunter2".as_slice(), &[key]].concat();
        let (ok, output) = prompt_run(test, "read", &[(PROMPT, &keys)]);
        assert!(!ok, "{output:?}");
        assert!(
            output.contains("crystal: passphrase entry cancelled"),
            "{output:?}"
        );
        assert!(
            !output.contains("hunter2") && !output.contains("len="),
            "{output:?}"
        );
    }
}

/// Over 1024 bytes is refused, not truncated.
#[test]
fn prompt_refuses_overlong_input() {
    let keys = [vec![b'a'; 1100], b"\r".to_vec()].concat();
    let (ok, output) = prompt_run("prompt_overlong", "read", &[(PROMPT, &keys)]);
    assert!(!ok, "{output:?}");
    assert!(
        output.contains("crystal: passphrase longer than 1024 bytes"),
        "{output:?}"
    );
    assert!(!output.contains("aaaa"), "{output:?}");
}

/// An exit signal during the prompt cancels it (and the buffer is dropped,
/// so zeroized). `exec` keeps the shell's pid, which the background job
/// signals.
#[test]
fn prompt_exit_signal_cancels() {
    let home = fresh_home("prompt_signal");
    make_config(&home);
    let cmd = format!("(sleep 1; kill -TERM $$) & exec '{BIN}' --prompt-for-test read diary");
    let (ok, output) = pty_dialog(&cmd, &home, &[(PROMPT, b"hunter2")]);
    assert!(!ok, "{output:?}");
    assert!(
        output.contains("crystal: passphrase entry cancelled"),
        "{output:?}"
    );
    assert!(!output.contains("hunter2"), "{output:?}");
}
