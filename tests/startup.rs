//! Child-process tests of the real binary's startup order (docs/design/startup.md).
//!
//! They need RLIMIT_MEMLOCK at the floor (docs/guide/setup.md; CI raises it).
//! Below it the binary stops at step 4 and these fail with the memlock
//! message instead.
//!
//! Every run gets its own HOME under cargo's test directory, so the tests
//! never create or touch the real ~/.config/crystal or
//! ~/.local/state/crystal.

mod common;

use common::*;
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Creates HOME/.config/crystal, both levels 0700, without a config.age:
/// enough for startup steps 1 to 7. Tests of those steps, signals and the
/// terminal guard pass the debug-only `--skip-unlock-for-test`, so they
/// reach the guard without a vault; tests/vault.rs covers unlock.
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_without_terminal() {
    let home = fresh_home("no_terminal");
    make_config(&home);
    // New session: no controlling terminal, even when `cargo test` itself
    // runs in an interactive shell.
    let out = Command::new("setsid")
        .args(["--wait", BIN, "--skip-unlock-for-test", "diary"])
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn starts_in_terminal_with_redirected_stdio() {
    let home = fresh_home("redirected_stdio");
    make_config(&home);
    let (ok, output) = run_in_pty(
        "--skip-unlock-for-test diary </dev/null >/dev/null",
        Some(&home),
    );
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_inherited_file_descriptor() {
    let home = fresh_home("inherited_fd");
    make_config(&home);
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary 5</dev/null", Some(&home));
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn running_binary_is_hardened() {
    let home = fresh_home("hardened");
    make_config(&home);
    let cmd = format!(
        "'{BIN}' --skip-unlock-for-test diary & p=$!; sleep 1; echo; \
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn config_dir_checks() {
    let home = fresh_home("config_dir");
    let config = home.join(".config/crystal");

    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
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
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
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
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
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
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
    assert!(
        !ok && output.contains("a parent of ~/.config/crystal is a symlink"),
        "{output}"
    );
    // Refused before the state directory was touched.
    assert!(!home.join(".local").exists());
}

/// Step 7: a fresh state directory is created with every level 0700.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn creates_state_dir_with_mode_0700() {
    let home = fresh_home("state_dir_fresh");
    make_config(&home);
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
    assert!(ok, "{output}");
    for dir in [".local", ".local/state", ".local/state/crystal"] {
        assert_eq!(mode(&home.join(dir)), 0o700, "{dir}");
    }
    // A second start accepts the directory it created.
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
    assert!(ok, "{output}");
}

/// Step 7: an existing state directory with a looser mode is refused,
/// not silently fixed.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_state_dir_with_loose_mode() {
    let home = fresh_home("state_dir_loose");
    make_config(&home);
    let state = home.join(".local/state/crystal");
    std::fs::create_dir_all(&state).unwrap();
    set_mode(&state, 0o755);
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_state_dir_symlink() {
    let home = fresh_home("state_dir_symlink");
    make_config(&home);
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(home.join(".local/state")).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    set_mode(&elsewhere, 0o700);
    symlink(&elsewhere, home.join(".local/state/crystal")).unwrap();
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
    assert!(
        !ok && output.contains("is a symlink or not a directory"),
        "{output}"
    );
}

/// Step 7: a symlink in a parent of the state directory is refused too, so
/// nothing can redirect where the generation counter is kept. Nothing is
/// created at the symlink's target.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_state_dir_parent_symlink() {
    let home = fresh_home("state_parent_symlink");
    make_config(&home);
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(home.join(".local")).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    set_mode(&elsewhere, 0o700);
    symlink(&elsewhere, home.join(".local/state")).unwrap();
    let (ok, output) = run_in_pty("--skip-unlock-for-test diary", Some(&home));
    assert!(
        !ok && output.contains("a parent of ~/.local/state/crystal is a symlink"),
        "{output}"
    );
    assert!(!elsewhere.join("crystal").exists());
}

/// Steps 3 and 7: HOME must be set and absolute.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_missing_or_relative_home() {
    for home in [None, Some(Path::new("relative/home"))] {
        let (ok, output) = run_in_pty("--skip-unlock-for-test diary", home);
        assert!(
            !ok && output.contains("HOME is unset or not an absolute path"),
            "{output}"
        );
    }
}

/// Phase 2 end to end: `crystal init` creates the documented layout, the
/// recovery code alone recovers the vault, and the vault can be removed
/// from and added back to the config. The passphrase never appears in the
/// output.
#[test]
fn init_remove_add_end_to_end() {
    use std::str::FromStr;
    let home = private_home("init_e2e");

    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &init_steps());
    assert!(ok, "{output}");
    assert!(!output.contains(PASS), "passphrase shown: {output:?}");
    // The code typed back was the code shown; age's scrypt passphrase is
    // its 28 symbols without dashes.
    let code = recovery_code_in(&output).replace('-', "");

    // Acceptance: the layout matches docs/design/storage.md.
    let vault = home.join("Vaults/diary");
    assert_eq!((mode(&home.join("Vaults")), mode(&vault)), (0o700, 0o700));
    let mut files: Vec<String> = std::fs::read_dir(&vault)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "VERSION",
            "identity.age",
            "identity.backup.age",
            "vault.age"
        ]
    );
    for file in &files {
        assert_eq!(mode(&vault.join(file)), 0o600, "{file}");
    }
    assert_eq!(std::fs::read(vault.join("VERSION")).unwrap(), b"1\n");
    assert_eq!(mode(&home.join(".config/crystal/config.age")), 0o600);
    let state: Vec<String> = std::fs::read_dir(home.join(".local/state/crystal"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(state.len(), 2, "{state:?}");
    assert!(state.iter().any(|f| f == "config.gen"), "{state:?}");
    assert!(
        state.iter().any(|f| f.len() == 64 + 4
            && f.ends_with(".gen")
            && f[..64]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
        "<vault_id hex>.gen: {state:?}"
    );

    // Acceptance: identity.backup.age recovers the vault without the
    // passphrase, and holds the same identity as identity.age.
    let by_code = age::scrypt::Identity::new(code.into());
    let from_backup = age_decrypt(&by_code, &vault.join("identity.backup.age"));
    let recovered =
        age::x25519::Identity::from_str(std::str::from_utf8(&from_backup).unwrap()).unwrap();
    age_decrypt(&recovered, &vault.join("vault.age"));
    let scrypt = age::scrypt::Identity::new(PASS.to_owned().into());
    assert_eq!(
        age_decrypt(&scrypt, &vault.join("identity.age")),
        from_backup
    );

    // The same target again is refused before any prompt.
    let (ok, output) = run_in_pty("init diary", Some(&home));
    assert!(
        !ok && output.contains("the vault directory already exists"),
        "{output}"
    );

    // The same name elsewhere passes the target checks, then is refused
    // once the config is unlocked.
    let other = home.join("other");
    let steps = vec![(PROMPT, format!("{PASS}\r").into_bytes())];
    let (ok, output) = dialog(
        &format!("'{BIN}' init diary --at '{}'", other.display()),
        &home,
        &steps,
    );
    assert!(!ok && output.contains("already registered"), "{output}");
    assert!(!other.exists());

    // `crystal remove` keeps the vault's files; `crystal add` registers it again.
    let (ok, output) = dialog(&format!("'{BIN}' remove diary"), &home, &steps);
    assert!(ok, "{output}");
    assert_eq!(std::fs::read_dir(&vault).unwrap().count(), 4);
    let config = home.join(".config/crystal/config.age");
    let decrypted = age_decrypt(&scrypt, &config);
    assert!(
        !decrypted.windows(5).any(|w| w == b"diary"),
        "still registered"
    );

    let (ok, output) = dialog(
        &format!("'{BIN}' add diary '{}'", vault.display()),
        &home,
        &steps,
    );
    assert!(ok, "{output}");
    let decrypted = age_decrypt(&scrypt, &config);
    assert!(
        decrypted.windows(5).any(|w| w == b"diary"),
        "not registered"
    );
}

/// Every interruption before creation leaves nothing behind: cancelling at
/// the passphrase, the repeat, the recovery code screen or its
/// confirmation creates no vault and no config.age.
#[test]
fn init_cancelled_leaves_no_vault() {
    let all = init_steps();
    for cut in 0..all.len() {
        let home = private_home(&format!("init_cancel_{cut}"));
        let mut steps = all[..cut].to_vec();
        steps.push((all[cut].0, b"abc\x03".to_vec()));
        let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &steps);
        assert!(!ok && output.contains("cancelled"), "step {cut}: {output}");
        assert!(!home.join("Vaults").exists(), "step {cut}");
        assert!(
            !home.join(".config/crystal/config.age").exists(),
            "step {cut}"
        );
    }
}

/// `crystal add` checks the directory before asking for any passphrase: a vault
/// below a world-writable directory is refused at once, and no config.age
/// is written. (Only the config and state directories are created.)
#[test]
fn add_refuses_unsafe_path_before_prompting() {
    let home = fresh_home("add_unsafe");
    let vault = home.join("shared/v");
    std::fs::create_dir_all(&vault).unwrap();
    set_mode(&home, 0o700);
    set_mode(&home.join("shared"), 0o777);
    set_mode(&vault, 0o700);
    std::fs::write(vault.join("VERSION"), b"1\n").unwrap();

    let (ok, output) = run_in_pty(&format!("add v '{}'", vault.display()), Some(&home));
    assert!(!ok, "{output}");
    assert!(output.contains("writable by group or others"), "{output}");
    assert!(!output.contains("assphrase"), "prompted first: {output}");
    assert!(!home.join(".config/crystal/config.age").exists());
}

/// `crystal add` of a directory that does not exist fails without a prompt.
#[test]
fn add_refuses_missing_directory() {
    let home = fresh_home("add_missing");
    set_mode(&home, 0o700);
    let (ok, output) = run_in_pty("add v ./no-such-dir", Some(&home));
    assert!(
        !ok && output.contains("cannot resolve the vault directory"),
        "{output}"
    );
    assert!(!home.join(".config/crystal/config.age").exists());
}

/// `crystal remove` never creates anything: without a config directory it fails
/// like a normal run; with one but no config.age, the name is unknown.
#[test]
fn remove_without_config() {
    let home = fresh_home("remove_none");
    let (ok, output) = run_in_pty("remove v", Some(&home));
    assert!(
        !ok && output.contains("~/.config/crystal does not exist"),
        "{output}"
    );
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);

    make_config(&home);
    let (ok, output) = run_in_pty("remove v", Some(&home));
    assert!(
        !ok && output.contains("no vault with that name is registered"),
        "{output}"
    );
    assert!(!home.join(".config/crystal/config.age").exists());
}

/// Step 7: the guard enters the alternate screen, clears it before leaving,
/// and restores the exact terminal settings it found.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn terminal_guard_restores_terminal() {
    let home = fresh_home("terminal_guard");
    make_config(&home);
    // echo: Crystal prints no newline, so separate its output from stty's.
    let cmd = format!("stty -g; '{BIN}' --skip-unlock-for-test diary; echo; stty -g");
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn terminal_hangup_exits_cleanly() {
    let home = fresh_home("hangup");
    make_config(&home);
    let status = home.join("status");
    let cmd = format!(
        "trap '' HUP; '{BIN}' --skip-unlock-for-test diary; echo $? > '{}'",
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn kill_9_leaves_no_new_files() {
    let home = fresh_home("kill_9");
    make_config(&home);
    let listing = |name: &str| home.with_extension(name);
    let (before, after) = (listing("before"), listing("after"));
    let cmd = format!(
        "'{BIN}' --skip-unlock-for-test diary & p=$!; sleep 1; find . | sort > '{}'; kill -9 $p; wait $p; s=$?; \
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
        "stty -g; '{BIN}' --skip-unlock-for-test diary & p=$!; sleep 1; kill -{signal} $p; wait $p; s=$?; \
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn suspend_restores_terminal_and_resume_reenters() {
    let home = fresh_home("suspend");
    make_config(&home);
    let cmd = format!(
        "stty -g; '{BIN}' --skip-unlock-for-test diary & p=$!; sleep 1; kill -TSTP $p; sleep 0.5; \
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

/// SIGWINCH wakes the loop for a redraw and nothing else: a burst of them
/// is drained (the process goes back to sleep instead of spinning), it
/// keeps running, and SIGTERM then exits cleanly. Leaving the alternate
/// screen shows the cursor again.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn resize_signal_redraws_and_keeps_running() {
    let home = fresh_home("resize");
    make_config(&home);
    let cmd = format!(
        "'{BIN}' --skip-unlock-for-test diary & p=$!; sleep 1; \
         for i in 1 2 3 4 5; do kill -WINCH $p; done; sleep 0.5; \
         echo; echo state=$(cut -d' ' -f3 /proc/$p/stat); \
         kill -TERM $p; wait $p; s=$?; echo; echo exit=$s"
    );
    let (ok, output) = pty(&cmd, Some(&home), b"");
    assert!(ok, "{output}");
    assert!(output.contains("state=S"), "not waiting: {output:?}");
    assert!(output.contains("exit=0"), "{output:?}");
    assert!(
        output.contains("\x1b[?25h\x1b[2J\x1b[?1049l"),
        "cursor shown on leave: {output:?}"
    );
}

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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn prompt_discards_keys_typed_before_it() {
    let home = fresh_home("prompt_typeahead");
    make_config(&home);
    let cmd = format!("sleep 1; '{BIN}' --prompt-for-test read diary");
    let steps: [(&str, &[u8]); 2] = [("", b"early"), (PROMPT, b"pw\r")];
    let (ok, output) = pty_dialog(&cmd, &home, &steps);
    assert!(ok, "{output:?}");
    assert!(output.contains("len=2"), "{output:?}");
}

/// `crystal init`'s variant: two matching entries are accepted, different ones
/// refused.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn prompt_confirm_requires_matching_entries() {
    let pass = b"twenty-characters-ok\r";
    let same: [(&str, &[u8]); 2] = [(PROMPT, pass), (REPEAT, pass)];
    let (ok, output) = prompt_run("prompt_confirm", "confirm", &same);
    assert!(ok && output.contains("len=20"), "{output:?}");

    let differ: [(&str, &[u8]); 2] = [(PROMPT, pass), (REPEAT, b"twenty-characters-oK\r")];
    let (ok, output) = prompt_run("prompt_mismatch", "confirm", &differ);
    assert!(!ok, "{output:?}");
    assert!(
        output.contains("crystal: the passphrases do not match"),
        "{output:?}"
    );
}

/// decision 0028: a new passphrase needs 20 characters, counted as
/// characters, not bytes. A short one is refused before the repeat prompt.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn prompt_new_passphrase_minimum() {
    let short: [(&str, &[u8]); 1] = [(PROMPT, b"nineteen-characters\r")];
    let (ok, output) = prompt_run("prompt_short", "confirm", &short);
    assert!(!ok, "{output:?}");
    assert!(
        output.contains("a new passphrase needs at least 20 characters"),
        "{output:?}"
    );
    assert!(!output.contains(REPEAT), "asked to repeat: {output:?}");
    assert!(!output.contains("nineteen"), "{output:?}");

    // 19 two-byte characters (38 bytes) are still too few.
    let wide = format!("{}\r", "ü".repeat(19));
    let (ok, output) = prompt_run("prompt_short_wide", "confirm", &[(PROMPT, wide.as_bytes())]);
    assert!(
        !ok && output.contains("at least 20 characters"),
        "{output:?}"
    );

    let wide = format!("{}\r", "ü".repeat(20));
    let steps: [(&str, &[u8]); 2] = [(PROMPT, wide.as_bytes()), (REPEAT, wide.as_bytes())];
    let (ok, output) = prompt_run("prompt_wide_ok", "confirm", &steps);
    assert!(ok && output.contains("len=40"), "{output:?}");
}

/// Ctrl+C and Esc cancel; the error names no typed byte.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
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
