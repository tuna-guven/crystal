//! Vault I/O: on-disk format, Padmé padding, generation counter, pin and
//! the atomic writer. Writes only ciphertext, or the fixed public
//! `VERSION`, `*.gen` and `*.pin` formats (`atomic::PublicBytes`). `codec` turns notes into the
//! plaintext bytes that `crypto` encrypts (and back), in zeroizing
//! buffers, and never touches a file.

pub(crate) mod atomic;
pub(crate) mod codec;
pub(crate) mod format;
pub(crate) mod generation;
pub(crate) mod padme;
pub(crate) mod pin;
pub(crate) mod public_file;
pub(crate) mod vault_dir;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{CWD, Mode, OFlags, fstat, mkdirat, openat};
use rustix::io::Errno;
use rustix::process::geteuid;

pub(crate) const OPEN_DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// A vault name: the directory name under ~/.local/share/crystal
/// (docs/decisions/0032-fixed-locations.md). 1 to 64 characters
/// from `A-Z a-z 0-9 - _`, starting with a letter or digit, and not a
/// command word. No `/`, `..`, hidden names or option look-alikes can
/// exist as a value.
#[derive(PartialEq, Eq)]
pub struct VaultName(OsString);

/// Never shows the name: it came from argv, and a mistyped passphrase made
/// of letters and digits passes validation. Debug exists only so types
/// holding a VaultName can be compared in tests with assert_eq!.
impl fmt::Debug for VaultName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultName(..)")
    }
}

impl VaultName {
    pub const MAX_LEN: usize = 64;
    /// The command words (`crystal init`, `rm`, `recover`, `check-code`,
    /// `help`): a vault with one of these names could not be opened as
    /// `crystal NAME`.
    pub const RESERVED: [&str; 5] = ["init", "rm", "recover", "check-code", "help"];

    pub fn new(name: OsString) -> Option<Self> {
        let bytes = name.as_bytes();
        let first_ok = bytes.first().is_some_and(u8::is_ascii_alphanumeric);
        let rest_ok = bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_');
        let reserved = Self::RESERVED.iter().any(|word| bytes == word.as_bytes());
        (first_ok && rest_ok && !reserved && bytes.len() <= Self::MAX_LEN).then_some(Self(name))
    }

    /// The name as text: validation allows only ASCII letters, digits, `-`
    /// and `_`, so it always is.
    pub fn as_str(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }
}

/// Which private directory a check failed on, and how. Messages name the
/// fixed directory, never a value from argv.
#[derive(Debug)]
pub struct PrivateDirError {
    dir: &'static str,
    problem: Problem,
}

#[derive(Debug)]
enum Problem {
    NoHome,
    Missing,
    /// `~/.local/share/crystal` does not exist: no vault was created yet.
    NoVaults,
    Create(Errno),
    NotARealDirectory,
    Open(Errno),
    Stat(Errno),
    NotOwned,
    BadMode(u32),
    UnsafeParent,
}

impl fmt::Display for PrivateDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dir = self.dir;
        match self.problem {
            Problem::NoHome => write!(f, "HOME is unset or not an absolute path"),
            Problem::Missing => write!(f, "{dir} does not exist; see docs/guide/setup.md"),
            Problem::NoVaults => write!(
                f,
                "{dir} does not exist: there is no vault yet; create one with crystal init NAME"
            ),
            Problem::Create(errno) => write!(f, "cannot create {dir} ({errno})"),
            Problem::NotARealDirectory => write!(f, "{dir} is a symlink or not a directory"),
            Problem::Open(errno) => write!(f, "cannot open {dir} ({errno})"),
            Problem::Stat(errno) => write!(f, "cannot check {dir} ({errno})"),
            Problem::NotOwned => write!(f, "{dir} is owned by another user"),
            Problem::UnsafeParent => write!(
                f,
                "a parent of {dir} is a symlink, not yours, or writable by group or others"
            ),
            Problem::BadMode(mode) => {
                write!(f, "{dir} has mode {mode:o}, need 700; run: chmod 700 {dir}")
            }
        }
    }
}

impl std::error::Error for PrivateDirError {}

const STATE_DIR: &str = "~/.local/state/crystal";
const DATA_DIR: &str = "~/.local/share/crystal";

/// HOME as an absolute path, or NoHome for `dir`.
fn home_dir(home: Option<&OsStr>, dir: &'static str) -> Result<PathBuf, PrivateDirError> {
    let no_home = PrivateDirError {
        dir,
        problem: Problem::NoHome,
    };
    let path = PathBuf::from(home.ok_or(no_home)?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(PrivateDirError {
            dir,
            problem: Problem::NoHome,
        })
    }
}

/// Why an open directory fd is not private: not ours, or not mode 0700.
enum NotPrivate {
    Stat(Errno),
    NotOwned,
    BadMode(u32),
}

/// Checks owner and mode on the open fd, not the path, so the checked
/// directory is the one in use.
fn check_private(fd: &OwnedFd) -> Result<(), NotPrivate> {
    let stat = fstat(fd).map_err(NotPrivate::Stat)?;
    if stat.st_uid != geteuid().as_raw() {
        return Err(NotPrivate::NotOwned);
    }
    let mode = stat.st_mode & 0o7777;
    if mode != 0o700 {
        return Err(NotPrivate::BadMode(mode));
    }
    Ok(())
}

/// Opens an existing directory `name` relative to `at` without following
/// a final symlink, then checks that it is ours and mode 0700.
fn open_private_dir(
    at: impl AsFd,
    name: &Path,
    dir: &'static str,
) -> Result<OwnedFd, PrivateDirError> {
    let fail = |problem| PrivateDirError { dir, problem };
    let fd = openat(at, name, OPEN_DIR_FLAGS, Mode::empty()).map_err(|errno| match errno {
        Errno::NOENT => fail(Problem::Missing),
        Errno::NOTDIR => fail(Problem::NotARealDirectory),
        other => fail(Problem::Open(other)),
    })?;
    check_private(&fd).map_err(|e| {
        fail(match e {
            NotPrivate::Stat(errno) => Problem::Stat(errno),
            NotPrivate::NotOwned => Problem::NotOwned,
            NotPrivate::BadMode(mode) => Problem::BadMode(mode),
        })
    })?;
    Ok(fd)
}

/// ~/.local/state/crystal: local-only state such as the generation counter
/// that detects rollback. Never synced. Must be ours and mode 0700, or
/// another user could reset the counter.
pub struct StateDir {
    fd: OwnedFd,
}

impl StateDir {
    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    /// Startup step 7. Created on first use: losing it only resets the
    /// rollback baseline to what the next unlock sees.
    pub fn open_or_create(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        Ok(Self {
            fd: open_home_subdir(
                home,
                STATE_DIR,
                &[".local", "state"],
                "crystal",
                Create::Yes,
            )?,
        })
    }
}

/// ~/.local/share/crystal: every vault, one directory each
/// (docs/decisions/0032-fixed-locations.md). Must be ours and mode 0700,
/// and reached without a symlink. A normal run only opens it, so a
/// mistyped command creates nothing; only `crystal init` creates it.
pub struct DataDir {
    fd: OwnedFd,
}

impl DataDir {
    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    /// Startup step 7. A missing directory means no vault exists yet.
    pub fn open(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        let fd = open_home_subdir(home, DATA_DIR, &[".local", "share"], "crystal", Create::No)
            .map_err(|e| match e.problem {
                Problem::Missing => PrivateDirError {
                    problem: Problem::NoVaults,
                    ..e
                },
                _ => e,
            })?;
        Ok(Self { fd })
    }

    /// For `crystal init` only: creates missing levels with mode 0700,
    /// then applies the same checks as `open`.
    pub fn create(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        Ok(Self {
            fd: open_home_subdir(home, DATA_DIR, &[".local", "share"], "crystal", Create::Yes)?,
        })
    }
}

/// Whether `open_home_subdir` may create missing directories.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Create {
    Yes,
    No,
}

/// Opens HOME/`parents`/`leaf` one directory at a time, each relative to
/// the previous fd. The `parents` must be real directories, ours, and not
/// writable by group or others, so nobody can redirect the walk; the
/// `leaf` must be ours and mode 0700. With `Create::Yes`, missing levels
/// are made with mode 0700 on the way; with `Create::No`, nothing is
/// created and a missing level is `Missing`. HOME itself is followed: on
/// Fedora Atomic, /home is a symlink to /var/home.
fn open_home_subdir(
    home: Option<&OsStr>,
    dir: &'static str,
    parents: &[&str],
    leaf: &str,
    create: Create,
) -> Result<OwnedFd, PrivateDirError> {
    let fail = |problem| PrivateDirError { dir, problem };
    let home = home_dir(home, dir)?;
    let home_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut parent =
        openat(CWD, &home, home_flags, Mode::empty()).map_err(|e| fail(Problem::Open(e)))?;
    for part in parents {
        if create == Create::Yes {
            create_dir(&parent, part).map_err(|e| fail(Problem::Create(e)))?;
        }
        parent = open_safe_parent(&parent, part).map_err(fail)?;
    }
    if create == Create::Yes {
        create_dir(&parent, leaf).map_err(|e| fail(Problem::Create(e)))?;
    }
    open_private_dir(&parent, Path::new(leaf), dir)
}

/// mkdir with mode 0700; an existing entry is fine (it is checked when
/// opened).
fn create_dir(parent: &OwnedFd, name: &str) -> Result<(), Errno> {
    match mkdirat(parent, name, Mode::RWXU) {
        Ok(()) | Err(Errno::EXIST) => Ok(()),
        Err(errno) => Err(errno),
    }
}

/// Whether an intermediate directory on a walk is safe: ours and not
/// writable by group or others, so nobody else can rename or replace what
/// lies beneath it. Any stricter mode is fine (~/.local is often 0755).
fn is_safe_parent(uid: u32, mode: u32, our_uid: u32) -> bool {
    uid == our_uid && mode & 0o022 == 0
}

/// Opens an intermediate directory: not a symlink, ours, and not writable
/// by group or others (`is_safe_parent`).
fn open_safe_parent(parent: &OwnedFd, name: &str) -> Result<OwnedFd, Problem> {
    let fd = openat(parent, name, OPEN_DIR_FLAGS, Mode::empty()).map_err(|errno| match errno {
        Errno::NOENT => Problem::Missing,
        Errno::NOTDIR => Problem::UnsafeParent,
        other => Problem::Open(other),
    })?;
    let stat = fstat(&fd).map_err(Problem::Stat)?;
    if !is_safe_parent(stat.st_uid, stat.st_mode, geteuid().as_raw()) {
        return Err(Problem::UnsafeParent);
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Option<VaultName> {
        VaultName::new(OsString::from(s))
    }

    #[test]
    fn vault_names_allowed() {
        for ok in ["diary", "a", "Work-2026", "my_notes", "9lives"] {
            assert!(name(ok).is_some(), "{ok}");
        }
        assert!(name(&"a".repeat(VaultName::MAX_LEN)).is_some());
    }

    #[test]
    fn vault_names_refused() {
        let too_long = "a".repeat(VaultName::MAX_LEN + 1);
        let bad = [
            "", ".", "..", "../x", "a/b", "/abs", ".hidden", "-opt", "_x", "a b", "a.b", "ü",
            "a\0b", &too_long,
        ];
        for bad in bad {
            assert!(name(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn vault_name_debug_hides_the_name() {
        let shown = format!("{:?}", name("hunter2").unwrap());
        assert_eq!(shown, "VaultName(..)");
    }

    /// A fresh, empty HOME under target/ (std::env::temp_dir is banned,
    /// and CARGO_TARGET_TMPDIR exists only for integration tests).
    fn fresh_home(test: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-homes")
            .join(test);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    // Only `crystal init` creates the data directory, every level 0700;
    // a second create accepts what the first one made.
    #[test]
    fn data_dir_create_makes_every_level_0700() {
        let home = fresh_home("data_create");
        DataDir::create(Some(home.as_os_str())).unwrap();
        for dir in [".local", ".local/share", ".local/share/crystal"] {
            assert_eq!(mode(&home.join(dir)), 0o700, "{dir}");
        }
        DataDir::create(Some(home.as_os_str())).unwrap();
        DataDir::open(Some(home.as_os_str())).unwrap();
    }

    // A normal run never creates it, not even ~/.local.
    #[test]
    fn data_dir_open_creates_nothing() {
        let home = fresh_home("data_open_missing");
        let err = DataDir::open(Some(home.as_os_str())).err().unwrap();
        assert!(matches!(err.problem, Problem::NoVaults), "{err:?}");
        assert!(err.to_string().contains("there is no vault yet"));
        assert!(!home.join(".local").exists());
    }

    // Directories owned by someone else are refused, at the last level and
    // on the way. Uses root-owned /usr and /usr/bin, so it means nothing
    // when the tests run as root.
    #[test]
    fn foreign_owned_directories_are_refused() {
        if geteuid().is_root() {
            return;
        }
        let root = Some(OsStr::new("/"));
        let leaf = open_home_subdir(root, DATA_DIR, &[], "usr", Create::No);
        let err = leaf.err().unwrap();
        assert!(matches!(err.problem, Problem::NotOwned), "{err:?}");

        let parent = open_home_subdir(root, DATA_DIR, &["usr"], "bin", Create::No);
        let err = parent.err().unwrap();
        assert!(matches!(err.problem, Problem::UnsafeParent), "{err:?}");
    }
}
