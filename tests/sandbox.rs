//! Sandbox acceptance through the real binary (docs/design/sandboxing.md,
//! section 9; decisions 0010 and 0059). Each test starts Crystal with the
//! debug-only `--sandbox-check-for-test CHECK`: Crystal enters the sandbox
//! as before a real unlock, tries the one forbidden action CHECK names and
//! prints `sandbox-check: denied errno=N` or `sandbox-check: ALLOWED`.
//! Enforcement cannot be undone, so every check is its own process.
//!
//! Needs a kernel with Landlock ABI 10 or newer, like Crystal itself, and
//! RLIMIT_MEMLOCK at the floor (docs/guide/setup.md).

mod common;

use common::*;
use std::path::{Path, PathBuf};

/// EACCES: what Landlock returns for a path outside its rules.
const EACCES: &str = "denied errno=13";

/// A fresh, private HOME with a vault directory `name` that holds only
/// `VERSION`: enough for startup step 7, which is all the checks need
/// before the sandbox. No passphrase, no scrypt.
fn home_with_vault(test: &str) -> PathBuf {
    let home = private_home(test);
    make_vault(&home, "diary");
    home
}

fn make_vault(home: &Path, name: &str) {
    let vault = home.join(".local/share/crystal").join(name);
    std::fs::create_dir_all(&vault).unwrap();
    for dir in [".local", ".local/share", ".local/share/crystal"] {
        set_mode(&home.join(dir), 0o700);
    }
    set_mode(&vault, 0o700);
    std::fs::write(vault.join("VERSION"), "2\n").unwrap();
}

/// Runs one check and returns its output line.
fn run_check(home: &Path, check: &str) -> String {
    let cmd = format!("'{BIN}' unlock --sandbox-check-for-test {check} diary");
    let (ok, output) = pty_dialog(&cmd, home, &[]);
    assert!(ok, "{output:?}");
    output
        .lines()
        .find(|l| l.contains("sandbox-check:"))
        .unwrap_or_else(|| panic!("no result line: {output:?}"))
        .trim()
        .to_owned()
}

/// A private key in the user's home, readable by the test (and so by any
/// unconfined process of this user), is unreachable inside the sandbox.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn reading_outside_the_vault_is_denied() {
    let home = home_with_vault("sandbox_read_outside");
    let ssh = home.join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("id_ed25519"), "fake key\n").unwrap();
    // Positive control: the file is there and readable without the sandbox.
    assert_eq!(
        std::fs::read_to_string(ssh.join("id_ed25519")).unwrap(),
        "fake key\n"
    );
    assert!(run_check(&home, "read-outside").ends_with(EACCES));
}

/// Creating a file anywhere but beneath the vault or the state directory
/// is denied, and nothing appears.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn writing_outside_the_vault_is_denied() {
    let home = home_with_vault("sandbox_write_outside");
    assert!(run_check(&home, "write-outside").ends_with(EACCES));
    assert!(!home.join("outside").exists());
}

/// Only the opened vault is reachable: another vault next to it, in the
/// same ~/.local/share/crystal, is not (decision 0032).
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn another_vault_is_unreachable() {
    let home = home_with_vault("sandbox_other_vault");
    make_vault(&home, "other");
    // Positive control: the other vault is there.
    assert!(home.join(".local/share/crystal/other/VERSION").is_file());
    assert!(run_check(&home, "other-vault").ends_with(EACCES));
}

/// Read from outside while Crystal waits inside the sandbox (Landlock
/// would deny Crystal its own /proc): no new privileges, a seccomp filter
/// in filter mode (2), exactly one filter, and the memory unreadable for
/// a same-user process. SIGTERM still ends it cleanly.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn running_sandbox_shows_in_proc_status() {
    let home = home_with_vault("sandbox_status");
    let cmd = format!(
        "'{BIN}' unlock --sandbox-check-for-test wait diary & p=$!; sleep 1; echo; \
         grep -E '^(NoNewPrivs|Seccomp|Seccomp_filters):' /proc/$p/status; \
         cat /proc/$p/mem >/dev/null 2>&1; echo mem_read=$?; \
         kill -TERM $p; wait $p; echo exit=$?"
    );
    let (ok, output) = pty_dialog(&cmd, &home, &[]);
    assert!(ok, "{output:?}");
    let field = |name: &str| -> String {
        let line = output
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("no {name}: {output:?}"));
        line[name.len()..].trim().to_owned()
    };
    assert_eq!(field("NoNewPrivs:"), "1", "{output:?}");
    assert_eq!(field("Seccomp:"), "2", "{output:?}");
    assert_eq!(field("Seccomp_filters:"), "1", "{output:?}");
    assert!(output.contains("mem_read=1"), "{output:?}");
    assert!(output.contains("exit=0"), "{output:?}");
}

/// 128 + SIGSYS (31): how the shell reports a seccomp kill.
const KILLED_BY_SIGSYS: &str = "exit=159";

/// Runs one check that seccomp must stop: the process dies by `SIGSYS`
/// before it can print a result line.
fn assert_killed(test: &str, check: &str) {
    let home = home_with_vault(test);
    let cmd = format!("'{BIN}' unlock --sandbox-check-for-test {check} diary; echo exit=$?");
    let (ok, output) = pty_dialog(&cmd, &home, &[]);
    assert!(ok, "{output:?}");
    assert!(output.contains(KILLED_BY_SIGSYS), "{check}: {output:?}");
    assert!(!output.contains("sandbox-check:"), "{check}: {output:?}");
}

/// Everything sandboxing section 6 says seccomp must stop, through the
/// calls Crystal's dependencies would make: a network socket, a socket
/// pair, a thread, an anonymous memory file, a directory, an ioctl
/// outside the five, a signal to another process, and exec.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn seccomp_kills_forbidden_syscalls() {
    for check in [
        "socket",
        "socketpair",
        "thread",
        "memfd",
        "mkdir",
        "ioctl",
        "kill-parent",
        "exec",
    ] {
        assert_killed(&format!("sandbox_kill_{check}"), check);
    }
}

/// Landlock alone, without seccomp, still stops the network (TCP, and UDP
/// since ABI 10, decision 0077), pathname UNIX sockets (D-Bus, Wayland),
/// directories, signals to other processes and exec: each layer holds on
/// its own (sandboxing section 1).
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn landlock_alone_stops_network_signals_and_exec() {
    // EACCES for the network and files, EPERM for the signal scope.
    let eperm = "denied errno=1";
    let home = home_with_vault("sandbox_landlock_alone");
    // A live socket the test itself can connect to (positive control).
    let sock = home.join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::os::unix::net::UnixStream::connect(&sock).unwrap();
    for (check, expected) in [
        ("landlock-tcp", EACCES),
        ("landlock-udp", EACCES),
        ("landlock-unix", EACCES),
        ("landlock-mkdir", EACCES),
        ("landlock-kill-parent", eperm),
        ("landlock-exec", EACCES),
    ] {
        let line = run_check(&home, check);
        assert!(line.ends_with(expected), "{check}: {line}");
    }
    assert!(!home.join(".local/share/crystal/diary/sub").exists());
}

/// Decisions 0010, 0077: without Landlock ABI 10, Crystal refuses to
/// start, before any passphrase is asked, and names the ABI it needs. The
/// debug-only `--hide-landlock-for-test` makes the kernel answer the
/// crate's ABI query with ENOSYS, as a kernel without Landlock does, so the
/// real refusal path runs. Positive control: the same vault without the option
/// reaches the prompt.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn refuses_to_start_without_landlock() {
    let home = home_with_vault("sandbox_no_landlock");
    let cmd = format!("'{BIN}' unlock --hide-landlock-for-test diary; echo exit=$?");
    let (ok, output) = pty_dialog(&cmd, &home, &[]);
    assert!(ok, "{output:?}");
    assert!(output.contains("exit=1"), "{output:?}");
    assert!(
        output.contains("no Landlock ABI 10 or newer") && output.contains("section 5.4"),
        "{output:?}"
    );
    assert!(!output.contains(PROMPT), "asked anyway: {output:?}");

    let cmd = format!("'{BIN}' unlock diary");
    let (_, output) = pty_dialog(&cmd, &home, &[(PROMPT, b"\x1b")]);
    assert!(output.contains(PROMPT), "{output:?}");
}

/// Decision 0059: `crystal passwd` enters the sandbox before its first
/// prompt, so identity.age and vault.age are parsed and rewritten inside
/// it. Read from outside while it waits at that prompt. No debug hook:
/// this runs in release builds too.
#[test]
fn passwd_waits_inside_the_sandbox() {
    let home = home_with_vault("sandbox_passwd");
    let cmd = format!(
        "'{BIN}' passwd diary & p=$!; sleep 1; echo; \
         grep -E '^(NoNewPrivs|Seccomp):' /proc/$p/status; \
         kill -TERM $p; wait $p; echo exit=$?"
    );
    let (ok, output) = pty_dialog(&cmd, &home, &[]);
    assert!(ok, "{output:?}");
    assert!(output.contains("Seccomp:\t2"), "{output:?}");
    assert!(output.contains("NoNewPrivs:\t1"), "{output:?}");
}
