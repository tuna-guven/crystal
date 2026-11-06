//! Child-process tests of the real binary's startup order (DESIGN.md).
//!
//! They need RLIMIT_MEMLOCK at the floor (docs/SETUP.md; CI raises it).
//! Below it the binary stops at step 4 and these fail with the memlock
//! message instead.
//!
//! Every run gets its own HOME under cargo's test directory, so the tests
//! never create or touch the real ~/Vaults or ~/.local/state/crystal.

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

/// Creates HOME/Vaults and the vault `name` inside it, both 0700, as
/// docs/SETUP.md tells users to until --init exists.
fn make_vault(home: &Path, name: &str) {
    let vaults = home.join("Vaults");
    std::fs::create_dir_all(vaults.join(name)).unwrap();
    set_mode(&vaults, 0o700);
    set_mode(&vaults.join(name), 0o700);
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
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
    let (ok, output) = run_in_pty("diary </dev/null >/dev/null", Some(&home));
    assert!(ok, "{output}");
}

/// Step 6: a secret on the command line is refused and never echoed.
#[test]
fn rejects_secret_argument_without_echoing_it() {
    let home = fresh_home("secret_argument");
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
    for name in ["../hunter2", "hunter2/x", ".hunter2"] {
        let (ok, output) = run_in_pty(&format!("'{name}'"), Some(&home));
        assert!(!ok && output.contains("vault name must be"), "{output}");
        assert!(!output.contains("hunter2"), "{output}");
    }
    assert!(!home.join(".local").exists());
}

/// Step 7: the vault must be a real directory inside ~/Vaults.
#[test]
fn vault_must_be_a_real_directory() {
    let home = fresh_home("vault_checks");
    make_vault(&home, "real");
    let vaults = home.join("Vaults");
    symlink(vaults.join("real"), vaults.join("link")).unwrap();
    std::fs::File::create(vaults.join("file")).unwrap();

    let (ok, output) = run_in_pty("real", Some(&home));
    assert!(ok, "{output}");

    for name in ["link", "file"] {
        let (ok, output) = run_in_pty(name, Some(&home));
        assert!(
            !ok && output.contains("vault is a symlink or not a directory"),
            "{output}"
        );
    }

    let (ok, output) = run_in_pty("missingvault", Some(&home));
    assert!(
        !ok && output.contains("no vault with that name in ~/Vaults"),
        "{output}"
    );
    // The name came from argv; errors must not echo it.
    assert!(!output.contains("missingvault"), "{output}");
}

/// Step 7: the vault directory itself must be ours and mode 0700, not only
/// ~/Vaults around it. The error never names the vault.
#[test]
fn refuses_vault_with_loose_mode() {
    let home = fresh_home("vault_loose");
    make_vault(&home, "hunter2");
    set_mode(&home.join("Vaults/hunter2"), 0o750);
    let (ok, output) = run_in_pty("hunter2", Some(&home));
    assert!(
        !ok && output.contains("vault directory has mode 750, need 700"),
        "{output}"
    );
    assert!(!output.contains("hunter2"), "{output}");
}

/// Step 2: an fd inherited from whatever started Crystal would survive
/// Landlock and seccomp, so Crystal refuses to start with one open.
#[test]
fn refuses_inherited_file_descriptor() {
    let home = fresh_home("inherited_fd");
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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

/// Step 7: ~/Vaults must exist (it is never created by opening), be a real
/// directory, and be ours with mode 0700.
#[test]
fn vaults_root_checks() {
    let home = fresh_home("vaults_root");

    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("~/Vaults does not exist"),
        "{output}"
    );
    assert!(
        !home.join("Vaults").exists(),
        "opening must not create ~/Vaults"
    );

    make_vault(&home, "diary");
    set_mode(&home.join("Vaults"), 0o755);
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("~/Vaults has mode 755, need 700"),
        "{output}"
    );

    let elsewhere = home.join("elsewhere");
    std::fs::rename(home.join("Vaults"), &elsewhere).unwrap();
    set_mode(&elsewhere, 0o700);
    symlink(&elsewhere, home.join("Vaults")).unwrap();
    let (ok, output) = run_in_pty("diary", Some(&home));
    assert!(
        !ok && output.contains("~/Vaults is a symlink or not a directory"),
        "{output}"
    );
}

/// Step 7: a fresh state directory is created with every level 0700.
#[test]
fn creates_state_dir_with_mode_0700() {
    let home = fresh_home("state_dir_fresh");
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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

/// Until Phase 2, --init says so plainly and creates nothing.
#[test]
fn init_is_not_implemented_yet() {
    let home = fresh_home("init_not_implemented");
    let (ok, output) = run_in_pty("--init newvault", Some(&home));
    assert!(
        !ok && output.contains("--init is not implemented yet"),
        "{output}"
    );
    assert!(!home.join("Vaults").exists() && !home.join(".local").exists());
}

/// Step 7: the guard enters the alternate screen, clears it before leaving,
/// and restores the exact terminal settings it found.
#[test]
fn terminal_guard_restores_terminal() {
    let home = fresh_home("terminal_guard");
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
/// cannot even restore the terminal (DESIGN.md lists both as accepted).
/// What every one of them must still do: never print the message, and
/// leave no core file. A panic inside Drop aborts only after the panic
/// hook ran, so it does restore the terminal.
#[test]
fn abort_paths_print_no_secret_and_leave_no_core() {
    // (kind, exit code, terminal restored): 134 = SIGABRT, 139 = SIGSEGV;
    // on musl, Rust's stack guard kills with SIGSEGV and prints nothing.
    for (kind, exit, restored) in [("panic-in-drop", 134, true), ("stack-overflow", 139, false)] {
        let home = fresh_home(&format!("abort_{kind}"));
        make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
    make_vault(&home, "diary");
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
