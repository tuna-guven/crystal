//! Helpers shared by the integration tests: a fresh HOME per test, file
//! modes, and driving the real binary in a pseudo-terminal.
//!
//! Each test crate uses a different subset, so unused ones are allowed.
#![allow(dead_code, reason = "each test file uses its own subset")]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_crystal");

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A fresh, empty HOME for one test.
pub fn fresh_home(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

pub fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Runs `cmd` in a new pseudo-terminal, in `home`. For each step, waits
/// until `wait_for` appears in the output (after the previous step's
/// match), then types `keys`; then collects the rest. Waiting matters:
/// keys typed before Crystal enters raw mode would be echoed by the
/// terminal itself. A reader thread lets each wait time out instead of
/// hanging the test run.
pub fn pty_dialog(cmd: &str, home: &Path, steps: &[(&str, &[u8])]) -> (bool, String) {
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
        if *keys == PAUSE_ONE_SECOND {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        if *keys == SYNC_DIARY {
            let dir = home.join(".local/share/crystal/diary");
            std::fs::copy(dir.join("vault.age"), dir.join("synced")).unwrap();
            std::fs::rename(dir.join("synced"), dir.join("vault.age")).unwrap();
            continue;
        }
        stdin.write_all(keys).unwrap();
        stdin.flush().unwrap();
    }
    while next(&mut output) {}
    drop(stdin);
    (child.0.wait().unwrap().success(), text(&output))
}

pub const PROMPT: &str = "Passphrase: ";

pub const REPEAT: &str = "Repeat passphrase: ";

pub const NEW_PROMPT: &str = "New passphrase";

/// The confirmation prompt of `crystal rm` (src/rm.rs, CONFIRM_PROMPT).
pub const RM_CONFIRM: &str = "Type the vault's name to confirm: ";

/// As the keys of a dialog step: type nothing, wait one second. For
/// timing tests such as auto-lock; the output is still collected.
pub const PAUSE_ONE_SECOND: &[u8] = b"\0pause one second\0";

/// As the keys of a dialog step: type nothing, replace the `diary`
/// vault's `vault.age` with a copy of itself, as a sync tool would (same
/// bytes, a new file): Crystal's next save must be refused.
pub const SYNC_DIARY: &[u8] = b"\0sync diary\0";

pub const PASS: &str = "correct-horse-battery";

/// A HOME whose own mode passes the vault walk (mode 0700).
pub fn private_home(test: &str) -> PathBuf {
    let home = fresh_home(test);
    set_mode(&home, 0o700);
    home
}

/// The answers for `crystal init`: the passphrase twice.
pub fn init_steps() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
    ]
}

pub fn dialog(cmd: &str, home: &Path, steps: &[(&str, Vec<u8>)]) -> (bool, String) {
    let steps: Vec<(&str, &[u8])> = steps.iter().map(|(w, k)| (*w, k.as_slice())).collect();
    pty_dialog(cmd, home, &steps)
}

/// Crystal's output as visible text: each cursor move (`ESC [ row ; col H`)
/// becomes a space and every other CSI sequence is removed, so words that
/// the frame writer drew one by one read as a sentence again.
pub fn visible(output: &str) -> String {
    let mut out = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut last = ' ';
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() || c == '~' {
                    last = c;
                    break;
                }
            }
            if last == 'H' && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}
