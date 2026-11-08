//! The atomic writer: the only way Crystal writes a file outside backups
//! (CLAUDE.md). docs/design/storage.md "Save sequence": write a temp file in the same
//! directory, fsync, rename over the target, fsync the directory. The
//! rename is the commit point.

use std::ffi::CString;
use std::fmt;
use std::os::fd::OwnedFd;

use rustix::fs::{AtFlags, Dir, Mode, OFlags, RenameFlags, fsync, openat, renameat_with, unlinkat};
use rustix::io::{Errno, write};

use crate::crypto::{Ciphertext, fill_random};

/// What `atomic_write` writes. Every caller must say which kind it has, and
/// neither kind can be built from arbitrary bytes outside its module.
pub enum Payload<'a> {
    Ciphertext(&'a Ciphertext),
    Public(&'a PublicBytes),
}

/// The only plaintext Crystal ever writes: the fixed public formats of
/// `VERSION` (format version), the `*.gen` generation counters and the
/// `*.pin` vault pins. The field is private; the three encoders below are
/// the only way to construct one.
pub struct PublicBytes(Vec<u8>);

impl PublicBytes {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The `VERSION` file: the vault format number in ASCII decimal and a
    /// newline, for example `1\n`. Nothing else.
    pub fn version() -> Self {
        Self(format!("{}\n", super::codec::FORMAT).into_bytes())
    }

    /// A `*.gen` counter: the last generation seen on this device, in
    /// ASCII decimal and a newline, for example `42\n`. Nothing else.
    pub fn generation(seen: u64) -> Self {
        Self(format!("{seen}\n").into_bytes())
    }

    /// A `NAME.pin` file: the pinned vault id in 64 lowercase hex digits
    /// and a newline. Nothing else.
    pub fn pin(vault_id: &crate::model::VaultId) -> Self {
        Self(format!("{}\n", vault_id.hex()).into_bytes())
    }

    /// Test builds only, until the real encoders exist.
    #[cfg(test)]
    pub fn for_test(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// One file name inside a directory fd: never a path. 1 to `MAX_LEN` ASCII
/// characters from `A-Z a-z 0-9 . - _`, not starting with `.`. So there is
/// no `/`, `.`, `..` or NUL, and no name can clash with a temp file
/// (`.<name>.tmp-<16 hex>`, which must still fit in NAME_MAX = 255).
#[derive(Debug, PartialEq, Eq)]
pub struct FileName(String);

impl FileName {
    /// 255 (NAME_MAX) minus the temp file's extra characters: 1 for the
    /// leading `.`, 5 for `.tmp-`, 16 for the random hex.
    pub const MAX_LEN: usize = 255 - 1 - 5 - 16;

    pub fn new(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        let first_ok = bytes.first().is_some_and(|b| *b != b'.');
        let rest_ok = bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        (first_ok && rest_ok && bytes.len() <= Self::MAX_LEN).then(|| Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The step that failed. Each has a fixed name for the error message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Removing a temp file left behind by a crash.
    Sweep,
    Random,
    Open,
    Write,
    Fsync,
    Rename,
    DirFsync,
}

/// Why a write failed: the step and the errno, never a file name or any
/// content. Before `Rename` fails, the target still holds its old content;
/// a `DirFsync` error means the new content is in place but may not
/// survive a power loss.
#[derive(Debug, PartialEq, Eq)]
pub struct WriteError {
    pub step: Step,
    pub errno: Errno,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let step = match self.step {
            Step::Sweep => "removing a stale temp file",
            Step::Random => "choosing a temp file name",
            Step::Open => "creating the temp file",
            Step::Write => "writing the temp file",
            Step::Fsync => "syncing the temp file",
            Step::Rename => "replacing the file",
            Step::DirFsync => "syncing the directory",
        };
        write!(f, "save failed while {step} ({})", self.errno)
    }
}

impl std::error::Error for WriteError {}

/// Writes `data` to `name` inside `dir` so that a crash or an error at any
/// step leaves either the old file or the new one, never a mix:
///
/// 1. remove `.<name>.tmp-<hex>` files a crash left behind (they are never
///    the live file, so this loses nothing);
/// 2. `openat(dir, ".<name>.tmp-<16 random hex>", O_WRONLY | O_CREAT |
///    O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600)`: a fresh file in the same
///    directory, so the rename needs no Landlock `REFER` (sandboxing 5.2);
/// 3. write everything, `fsync` it;
/// 4. `renameat2(dir, tmp, dir, name)`: the commit point. A symlink at
///    `name` is replaced, never followed;
/// 5. `fsync(dir)`, so the rename itself is on disk.
///
/// If a step before the rename fails, the temp file is unlinked.
pub fn atomic_write(dir: &OwnedFd, name: &FileName, data: Payload<'_>) -> Result<(), WriteError> {
    let bytes = match data {
        Payload::Ciphertext(c) => c.as_bytes(),
        Payload::Public(p) => p.as_bytes(),
    };
    run(Step::Sweep, name, || remove_stale_temps(dir, name))?;
    let tmp = run(Step::Random, name, || temp_name(name))?;
    let file = run(Step::Open, name, || {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        openat(dir, tmp.as_c_str(), flags, Mode::RUSR | Mode::WUSR)
    })?;
    let committed = write_all(&file, bytes, name)
        .and_then(|()| run(Step::Fsync, name, || fsync(&file)))
        .and_then(|()| {
            run(Step::Rename, name, || {
                renameat_with(
                    dir,
                    tmp.as_c_str(),
                    dir,
                    name.as_str(),
                    RenameFlags::empty(),
                )
            })
        });
    drop(file);
    if let Err(e) = committed {
        // Best effort: if this fails too, the next write's sweep removes it.
        let _ = unlinkat(dir, tmp.as_c_str(), AtFlags::empty());
        return Err(e);
    }
    run(Step::DirFsync, name, || fsync(dir))
}

/// Runs one step, mapping its errno to a `WriteError` for that step. In
/// test builds, an armed `FailAt` makes the step fail before it runs.
fn run<T>(
    step: Step,
    name: &FileName,
    op: impl FnOnce() -> Result<T, Errno>,
) -> Result<T, WriteError> {
    fail_point(step, name)?;
    op().map_err(|errno| WriteError { step, errno })
}

/// Writes all of `bytes`, retrying on short writes and EINTR.
fn write_all(file: &OwnedFd, bytes: &[u8], name: &FileName) -> Result<(), WriteError> {
    let mut done = 0;
    while done < bytes.len() {
        match write(file, &bytes[done..]) {
            Ok(n) => done += n,
            Err(Errno::INTR) => {}
            Err(errno) => {
                return Err(WriteError {
                    step: Step::Write,
                    errno,
                });
            }
        }
        // Test builds: fail with part (or all) of the data in the temp file.
        fail_point(Step::Write, name)?;
    }
    Ok(())
}

const TEMP_INFIX: &str = ".tmp-";
const TEMP_HEX_LEN: usize = 16;

/// `.<name>.tmp-<16 random hex>`. 64 random bits, so two writers (or a
/// leftover the sweep could not remove) never pick the same name; O_EXCL
/// refuses it if they do.
fn temp_name(name: &FileName) -> Result<CString, Errno> {
    let mut random = [0u8; 8];
    fill_random(&mut random)?;
    let hex = u64::from_le_bytes(random);
    let tmp = format!(".{}{TEMP_INFIX}{hex:016x}", name.as_str());
    // FileName has no NUL, so this cannot fail.
    CString::new(tmp).map_err(|_| Errno::INVAL)
}

/// Whether `entry` is a temp file of `name`: exactly `.<name>.tmp-` and 16
/// lowercase hex digits. Anything else in the directory is left alone.
pub(crate) fn is_temp_of(entry: &[u8], name: &FileName) -> bool {
    let Some(rest) = entry
        .strip_prefix(b".")
        .and_then(|r| r.strip_prefix(name.as_str().as_bytes()))
        .and_then(|r| r.strip_prefix(TEMP_INFIX.as_bytes()))
    else {
        return false;
    };
    rest.len() == TEMP_HEX_LEN && rest.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Unlinks the temp files of `name` that a killed writer left behind. A
/// crash (kill -9, power loss) cannot clean up after itself, so the next
/// write does. Lists the directory through a fresh fd for "." (the caller's
/// fd keeps no read position), collects first and unlinks after.
fn remove_stale_temps(dir: &OwnedFd, name: &FileName) -> Result<(), Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let listing = openat(dir, c".", flags, Mode::empty())?;
    let mut stale = Vec::new();
    for entry in Dir::new(listing)? {
        let entry = entry?;
        if is_temp_of(entry.file_name().to_bytes(), name) {
            stale.push(entry.file_name().to_owned());
        }
    }
    for tmp in stale {
        match unlinkat(dir, tmp.as_c_str(), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(errno) => return Err(errno),
        }
    }
    Ok(())
}

#[cfg(not(test))]
#[inline(always)]
fn fail_point(_step: Step, _name: &FileName) -> Result<(), WriteError> {
    Ok(())
}

/// The armed step, and the only file it fails for (`None`: every file).
#[cfg(test)]
type Armed = Option<(Step, Option<&'static str>)>;

#[cfg(test)]
thread_local! {
    static FAIL_AT: std::cell::Cell<Armed> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fail_point(step: Step, name: &FileName) -> Result<(), WriteError> {
    if let Some((armed, only)) = FAIL_AT.get()
        && armed == step
        && only.is_none_or(|file| file == name.as_str())
    {
        return Err(WriteError {
            step,
            errno: Errno::IO,
        });
    }
    Ok(())
}

/// Test builds only: while alive, `step` fails with EIO on this thread.
#[cfg(test)]
pub struct FailAt;

#[cfg(test)]
impl FailAt {
    /// `step` fails for every file.
    pub fn arm(step: Step) -> Self {
        FAIL_AT.set(Some((step, None)));
        Self
    }

    /// `step` fails only while writing `file`; other writes succeed. For
    /// tests of a sequence of writes, such as `crystal init`.
    pub fn arm_for(step: Step, file: &'static str) -> Self {
        FAIL_AT.set(Some((step, Some(file))));
        Self
    }
}

#[cfg(test)]
impl Drop for FailAt {
    fn drop(&mut self) {
        FAIL_AT.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    const TARGET: &str = "vault.age";

    /// A fresh, empty directory under target/ and an fd for it.
    fn fresh_dir(test: &str) -> (PathBuf, OwnedFd) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-atomic")
            .join(test);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        let fd = open_dir(&path);
        (path, fd)
    }

    fn open_dir(path: &Path) -> OwnedFd {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        openat(rustix::fs::CWD, path, flags, Mode::empty()).unwrap()
    }

    fn target() -> FileName {
        FileName::new(TARGET).unwrap()
    }

    fn save(dir: &OwnedFd, bytes: &[u8]) -> Result<(), WriteError> {
        let c = Ciphertext::for_test(bytes.to_vec());
        atomic_write(dir, &target(), Payload::Ciphertext(&c))
    }

    /// Sorted names in `path`.
    fn entries(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn file_names_allowed() {
        for ok in [
            "vault.age",
            "VERSION",
            "config.gen",
            "0123abcd.gen",
            "a",
            "a..b",
        ] {
            assert_eq!(FileName::new(ok).map(|n| n.0), Some(ok.to_owned()));
        }
        assert!(FileName::new(&"a".repeat(FileName::MAX_LEN)).is_some());
    }

    #[test]
    fn file_names_refused() {
        let too_long = "a".repeat(FileName::MAX_LEN + 1);
        let bad = [
            "",
            ".",
            "..",
            ".hidden",
            ".vault.age.tmp-0011",
            "a/b",
            "/abs",
            "../x",
            "a\0b",
            "a b",
            "ü",
            "a\\b",
            &too_long,
        ];
        for bad in bad {
            assert!(FileName::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn writes_ciphertext_and_public_bytes_with_mode_0600() {
        let (path, dir) = fresh_dir("write");
        save(&dir, b"ciphertext").unwrap();
        assert_eq!(fs::read(path.join(TARGET)).unwrap(), b"ciphertext");
        assert_eq!(mode(&path.join(TARGET)), 0o600);

        let version = FileName::new("VERSION").unwrap();
        let public = PublicBytes::for_test(b"1\n".to_vec());
        atomic_write(&dir, &version, Payload::Public(&public)).unwrap();
        assert_eq!(fs::read(path.join("VERSION")).unwrap(), b"1\n");
        assert_eq!(mode(&path.join("VERSION")), 0o600);

        save(&dir, b"").unwrap();
        assert_eq!(fs::read(path.join(TARGET)).unwrap(), b"");
        assert_eq!(entries(&path), ["VERSION", TARGET]);
    }

    // The old file, whatever its mode, is replaced by a new 0600 file.
    #[test]
    fn replaces_an_existing_target() {
        let (path, dir) = fresh_dir("replace");
        fs::write(path.join(TARGET), b"old content, longer than the new").unwrap();
        fs::set_permissions(path.join(TARGET), fs::Permissions::from_mode(0o644)).unwrap();
        save(&dir, b"new").unwrap();
        assert_eq!(fs::read(path.join(TARGET)).unwrap(), b"new");
        assert_eq!(mode(&path.join(TARGET)), 0o600);
        assert_eq!(entries(&path), [TARGET]);
    }

    // A symlink at the target name is replaced by a regular file; the file
    // it pointed to, in another directory, is untouched.
    #[test]
    fn symlinked_target_is_replaced_not_followed() {
        let (path, dir) = fresh_dir("symlink");
        let (other, _other_fd) = fresh_dir("symlink_other");
        fs::write(other.join("victim"), b"keep").unwrap();
        symlink(other.join("victim"), path.join(TARGET)).unwrap();
        save(&dir, b"new").unwrap();
        assert_eq!(fs::read(other.join("victim")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(path.join(TARGET)).unwrap().is_file());
        assert_eq!(fs::read(path.join(TARGET)).unwrap(), b"new");
    }

    // Acceptance: an error at any step leaves the old or the new file,
    // never a mix and never a temp file. Before the rename it is the old
    // one (or none, if there was none); a failed directory fsync comes
    // after the commit point, so the new one is in place.
    #[test]
    fn every_failing_step_leaves_old_or_new_and_no_temp() {
        let steps = [
            Step::Sweep,
            Step::Open,
            Step::Write,
            Step::Fsync,
            Step::Rename,
            Step::DirFsync,
        ];
        for step in steps {
            let (path, dir) = fresh_dir(&format!("fail_{step:?}"));
            save(&dir, b"old").unwrap();
            let err = {
                let _armed = FailAt::arm(step);
                save(&dir, b"new").unwrap_err()
            };
            assert_eq!(err.step, step);
            let expected: &[u8] = if step == Step::DirFsync {
                b"new"
            } else {
                b"old"
            };
            assert_eq!(fs::read(path.join(TARGET)).unwrap(), expected, "{step:?}");
            assert_eq!(entries(&path), [TARGET], "{step:?}");

            // Without an old file, a failure before the commit leaves none.
            let (path, dir) = fresh_dir(&format!("fail_new_{step:?}"));
            {
                let _armed = FailAt::arm(step);
                save(&dir, b"new").unwrap_err();
            }
            let expected: &[&str] = if step == Step::DirFsync {
                &[TARGET]
            } else {
                &[]
            };
            assert_eq!(entries(&path), expected, "{step:?}");
        }
    }

    // A crash leaves its temp file behind; the next write removes it, and
    // only temp files of the same name with the exact pattern.
    #[test]
    fn stale_temp_files_are_removed_by_the_next_write() {
        let (path, dir) = fresh_dir("stale");
        let stale = ".vault.age.tmp-0123456789abcdef";
        let keep = [
            ".VERSION.tmp-0123456789abcdef",
            ".vault.age.tmp-0123456789ABCDEF",
            ".vault.age.tmp-0123",
            ".vault.age.tmp-0123456789abcdef0",
            "notes.txt",
        ];
        for name in keep.iter().chain([&stale]) {
            fs::write(path.join(name), b"x").unwrap();
        }
        save(&dir, b"new").unwrap();
        let mut expected: Vec<&str> = keep.to_vec();
        expected.push(TARGET);
        expected.sort();
        assert_eq!(entries(&path), expected);
    }

    #[test]
    fn temp_names_match_the_sweep_pattern() {
        let name = target();
        let a = temp_name(&name).unwrap();
        let b = temp_name(&name).unwrap();
        assert!(is_temp_of(a.to_bytes(), &name), "{a:?}");
        assert_ne!(a, b);
        let longest = FileName::new(&"a".repeat(FileName::MAX_LEN)).unwrap();
        assert_eq!(temp_name(&longest).unwrap().to_bytes().len(), 255);
    }

    #[test]
    fn error_message_is_fixed_text() {
        let (path, dir) = fresh_dir("message");
        fs::create_dir(path.join(TARGET)).unwrap();
        let err = save(&dir, b"new").unwrap_err();
        assert_eq!(err.step, Step::Rename);
        assert_eq!(
            err.to_string(),
            format!("save failed while replacing the file ({})", err.errno)
        );
        assert_eq!(entries(&path), [TARGET]);
    }

    const KILL_DIR_ENV: &str = "CRYSTAL_TEST_ATOMIC_KILL_DIR";
    const KILL_SIZE: usize = 1 << 20;

    /// Child of `kill_9_mid_write_leaves_old_or_new`: writes all-A and
    /// all-B files in a loop until killed. Does nothing when run on its own.
    #[test]
    #[ignore = "child process of kill_9_mid_write_leaves_old_or_new"]
    fn kill_9_child_write_loop() {
        #[expect(
            clippy::disallowed_methods,
            reason = "test child: directory from parent"
        )]
        let Some(path) = std::env::var_os(KILL_DIR_ENV) else {
            return;
        };
        let dir = open_dir(Path::new(&path));
        loop {
            for byte in b"AB" {
                save(&dir, &vec![*byte; KILL_SIZE]).unwrap();
            }
        }
    }

    // Acceptance: kill -9 at a random moment in a write loop. The target is
    // always one whole version, and the next write removes the leftover
    // temp file. Runs this test binary again as the child.
    #[test]
    fn kill_9_mid_write_leaves_old_or_new() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let (path, dir) = fresh_dir("kill_9");
        let (all_a, all_b) = (vec![b'A'; KILL_SIZE], vec![b'B'; KILL_SIZE]);
        save(&dir, &all_a).unwrap();
        for round in 0..20 {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "vault::atomic::tests::kill_9_child_write_loop"])
                .args(["--ignored", "--test-threads=1"])
                .env(KILL_DIR_ENV, &path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(30 + 7 * round));
            child.kill().unwrap();
            let status = child.wait().unwrap();
            assert_eq!(status.signal(), Some(9), "child ended on its own: {status}");
            let content = fs::read(path.join(TARGET)).unwrap();
            assert!(content == all_a || content == all_b, "round {round}: mixed");
        }
        save(&dir, b"done").unwrap();
        assert_eq!(entries(&path), [TARGET]);
    }
}
