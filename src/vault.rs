//! Vault I/O: on-disk format, Padmé padding, generation counter and the
//! atomic writer. Writes only ciphertext, or the fixed public `VERSION` and
//! `*.gen` formats (`atomic::PublicBytes`). `codec` turns notes into the
//! plaintext bytes that `crypto` encrypts (and back), in zeroizing
//! buffers, and never touches a file.

mod atomic;
mod codec;
mod padme;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{CWD, Mode, OFlags, fstat, mkdirat, openat};
use rustix::io::Errno;
use rustix::process::geteuid;

const OPEN_DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// A vault name: the key in config.age, and the directory name under
/// ~/Vaults when --init creates a vault there. 1 to 64 characters from
/// `A-Z a-z 0-9 - _`, starting with a letter or digit. No `/`, `..`,
/// hidden names or option look-alikes can exist as a value.
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

    pub fn new(name: OsString) -> Option<Self> {
        let bytes = name.as_bytes();
        let first_ok = bytes.first().is_some_and(u8::is_ascii_alphanumeric);
        let rest_ok = bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_');
        (first_ok && rest_ok && bytes.len() <= Self::MAX_LEN).then_some(Self(name))
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
            Problem::Missing => write!(f, "{dir} does not exist; see docs/SETUP.md"),
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
const CONFIG_DIR: &str = "~/.config/crystal";

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
    #[expect(dead_code, reason = "holds the generation counter in Phase 2")]
    fd: OwnedFd,
}

impl StateDir {
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

/// ~/.config/crystal: holds config.age. Local only, never synced. Must be
/// ours and mode 0700, or another user could swap the config. A normal run
/// only opens it, so a mistyped command creates nothing; only --init and
/// --add create it. Closed after unlock (step 8): the sandbox never
/// reaches it.
pub struct ConfigDir {
    #[expect(dead_code, reason = "config.age is read and written in #13")]
    fd: OwnedFd,
}

impl ConfigDir {
    /// Startup step 7 for a normal run. A missing directory is an error.
    pub fn open(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        Ok(Self {
            fd: open_home_subdir(home, CONFIG_DIR, &[".config"], "crystal", Create::No)?,
        })
    }

    /// For --init and --add only: creates missing levels with mode 0700,
    /// then applies the same checks as `open`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "called by --init and --add (#13, #14)")
    )]
    pub fn create(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        Ok(Self {
            fd: open_home_subdir(home, CONFIG_DIR, &[".config"], "crystal", Create::Yes)?,
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

/// Opens an intermediate directory: not a symlink, ours, and not writable
/// by group or others. Any mode stricter than that is fine (~/.local is
/// often 0755).
fn open_safe_parent(parent: &OwnedFd, name: &str) -> Result<OwnedFd, Problem> {
    let fd = openat(parent, name, OPEN_DIR_FLAGS, Mode::empty()).map_err(|errno| match errno {
        Errno::NOENT => Problem::Missing,
        Errno::NOTDIR => Problem::UnsafeParent,
        other => Problem::Open(other),
    })?;
    let stat = fstat(&fd).map_err(Problem::Stat)?;
    if stat.st_uid != geteuid().as_raw() || stat.st_mode & 0o022 != 0 {
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

    // Only --init and --add create the config directory, every level 0700;
    // a second create accepts what the first one made.
    #[test]
    fn config_dir_create_makes_every_level_0700() {
        let home = fresh_home("config_create");
        ConfigDir::create(Some(home.as_os_str())).unwrap();
        for dir in [".config", ".config/crystal"] {
            assert_eq!(mode(&home.join(dir)), 0o700, "{dir}");
        }
        ConfigDir::create(Some(home.as_os_str())).unwrap();
        ConfigDir::open(Some(home.as_os_str())).unwrap();
    }

    // A normal run never creates it, not even ~/.config.
    #[test]
    fn config_dir_open_creates_nothing() {
        let home = fresh_home("config_open_missing");
        let err = ConfigDir::open(Some(home.as_os_str())).err().unwrap();
        assert!(matches!(err.problem, Problem::Missing), "{err:?}");
        assert!(err.to_string().contains("~/.config/crystal does not exist"));
        assert!(!home.join(".config").exists());

        std::fs::create_dir(home.join(".config")).unwrap();
        let err = ConfigDir::open(Some(home.as_os_str())).err().unwrap();
        assert!(matches!(err.problem, Problem::Missing), "{err:?}");
        assert!(!home.join(".config/crystal").exists());
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
        let leaf = open_home_subdir(root, CONFIG_DIR, &[], "usr", Create::No);
        let err = leaf.err().unwrap();
        assert!(matches!(err.problem, Problem::NotOwned), "{err:?}");

        let parent = open_home_subdir(root, CONFIG_DIR, &["usr"], "bin", Create::No);
        let err = parent.err().unwrap();
        assert!(matches!(err.problem, Problem::UnsafeParent), "{err:?}");
    }
}
