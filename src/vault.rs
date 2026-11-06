//! Vault I/O: on-disk format, Padmé padding, generation counter and the
//! atomic writer. Only ever handles ciphertext.

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

/// A vault name that is safe to open relative to ~/Vaults: 1 to 64
/// characters from `A-Z a-z 0-9 - _`, starting with a letter or digit.
/// No `/`, `..`, hidden names or option look-alikes can exist as a value.
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

const VAULTS_DIR: &str = "~/Vaults";
const STATE_DIR: &str = "~/.local/state/crystal";

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

/// ~/Vaults, the only place vaults live. Never created here: only --init
/// creates it (Phase 2). Used only to open one vault, then closed, so other
/// vaults stay unreachable for the rest of the session.
pub struct VaultsRoot {
    fd: OwnedFd,
}

impl VaultsRoot {
    /// Startup step 7.
    pub fn open(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        let path = home_dir(home, VAULTS_DIR)?.join("Vaults");
        Ok(Self {
            fd: open_private_dir(CWD, &path, VAULTS_DIR)?,
        })
    }
}

/// The vault directory, opened once at startup. Every later file operation
/// is relative to this fd, so renaming or swapping the path afterwards has
/// no effect on which directory Crystal uses.
pub struct VaultDir {
    #[expect(dead_code, reason = "used by the vault format in Phase 2")]
    fd: OwnedFd,
}

/// Says which check failed, never the name: it came from argv.
#[derive(Debug)]
pub enum OpenVaultError {
    NoSuchVault,
    NotARealDirectory,
    Other(Errno),
    NotOwned,
    BadMode(u32),
}

impl fmt::Display for OpenVaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchVault => write!(f, "no vault with that name in {VAULTS_DIR}"),
            Self::NotARealDirectory => write!(f, "vault is a symlink or not a directory"),
            Self::Other(errno) => write!(f, "cannot open vault directory ({errno})"),
            Self::NotOwned => write!(f, "vault directory is owned by another user"),
            Self::BadMode(mode) => write!(
                f,
                "vault directory has mode {mode:o}, need 700; run: chmod 700 {VAULTS_DIR}/<name>"
            ),
        }
    }
}

impl std::error::Error for OpenVaultError {}

impl VaultDir {
    /// Startup step 7: open the vault relative to the root fd. O_NOFOLLOW
    /// refuses a symlinked vault; with O_DIRECTORY Linux reports it as
    /// ENOTDIR, the same as a regular file, so both share one error.
    pub fn open(root: &VaultsRoot, name: &VaultName) -> Result<Self, OpenVaultError> {
        let fd = openat(
            root.fd.as_fd(),
            name.0.as_os_str(),
            OPEN_DIR_FLAGS,
            Mode::empty(),
        )
        .map_err(|errno| match errno {
            Errno::NOENT => OpenVaultError::NoSuchVault,
            Errno::NOTDIR => OpenVaultError::NotARealDirectory,
            other => OpenVaultError::Other(other),
        })?;
        // The 0700 root already shields it today; vaults outside ~/Vaults
        // (Phase 2, config file) will rely on this check alone.
        check_private(&fd).map_err(|e| match e {
            NotPrivate::Stat(errno) => OpenVaultError::Other(errno),
            NotPrivate::NotOwned => OpenVaultError::NotOwned,
            NotPrivate::BadMode(mode) => OpenVaultError::BadMode(mode),
        })?;
        Ok(Self { fd })
    }
}

/// ~/.local/state/crystal: local-only state such as the generation counter
/// that detects rollback. Never synced. Must be ours and mode 0700, or
/// another user could reset the counter.
pub struct StateDir {
    #[expect(dead_code, reason = "holds the generation counter in Phase 2")]
    fd: OwnedFd,
}

impl StateDir {
    /// Startup step 7. Walks from HOME one directory at a time, relative
    /// to the previous fd, creating missing ones with mode 0700. `.local`
    /// and `state` must be real directories, ours, and not writable by
    /// group or others, so nobody can redirect the walk; `crystal` is then
    /// checked like ~/Vaults. HOME itself is followed: on Fedora Atomic,
    /// /home is a symlink to /var/home.
    pub fn open_or_create(home: Option<&OsStr>) -> Result<Self, PrivateDirError> {
        let fail = |problem| PrivateDirError {
            dir: STATE_DIR,
            problem,
        };
        let home = home_dir(home, STATE_DIR)?;
        let home_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let mut parent =
            openat(CWD, &home, home_flags, Mode::empty()).map_err(|e| fail(Problem::Open(e)))?;
        for part in [".local", "state"] {
            create_dir(&parent, part).map_err(|e| fail(Problem::Create(e)))?;
            parent = open_safe_parent(&parent, part).map_err(fail)?;
        }
        create_dir(&parent, "crystal").map_err(|e| fail(Problem::Create(e)))?;
        Ok(Self {
            fd: open_private_dir(&parent, Path::new("crystal"), STATE_DIR)?,
        })
    }
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
}
