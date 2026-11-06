//! Opening a vault directory named by config.age (docs/design/storage.md,
//! "Config file"). The path is treated as attacker-written: the config is
//! authenticated only as strongly as its recipient (anyone can encrypt to
//! a public key), so it can point Crystal at a vault but must never widen
//! the sandbox beyond one checked, private directory.
//!
//! Identity checks (is it HOME, the config or state directory, inside or
//! above them?) compare `(st_dev, st_ino)` of opened fds, never strings,
//! so no spelling, bind mount or alias can trick them.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "unlock and `crystal add` (#13, #15)")
)]

use std::ffi::OsStr;
use std::fmt;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;

use rustix::fs::{CWD, Mode, OFlags, fstat, openat};
use rustix::io::Errno;
use rustix::process::geteuid;

use super::format::{VersionError, read_version};
use super::{ConfigDir, OPEN_DIR_FLAGS, Owners, StateDir, is_safe_parent};

/// Linux PATH_MAX, including the terminating NUL.
const MAX_PATH: usize = 4096;

/// More `..` steps than any real directory tree has: stops a loop on a
/// filesystem that misreports its root.
const MAX_DEPTH: usize = 4096;

/// A directory's identity: `(st_dev, st_ino)`.
type DirId = (u64, u64);

/// Why a vault path was refused. Fixed text plus, at most, which component
/// of the path (by position) failed; never the path itself.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenVaultError {
    /// Not absolute, or has an empty, `.` or `..` component, a NUL byte,
    /// or is longer than PATH_MAX.
    BadPath,
    /// A component does not exist.
    Missing {
        component: usize,
    },
    /// A component is a symlink or not a directory.
    NotARealDirectory {
        component: usize,
    },
    /// A directory on the way is owned by someone other than root or us,
    /// or is writable by group or others.
    UnsafeParent {
        component: usize,
    },
    /// The vault directory is not ours.
    NotOwned,
    /// The vault directory's mode is not 0700.
    BadMode(u32),
    /// The vault directory is HOME.
    IsHome,
    /// The vault directory is the config or state directory, or inside one.
    InsideCrystalDir,
    /// The vault directory contains the config or state directory.
    ContainsCrystalDir,
    /// `VERSION` is missing, not a regular file, malformed or unknown.
    Version(VersionError),
    Open(Errno),
    Stat(Errno),
}

impl fmt::Display for OpenVaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPath => write!(
                f,
                "the vault path must be absolute, with no empty, '.' or '..' parts"
            ),
            Self::Missing { component } => {
                write!(f, "part {component} of the vault path does not exist")
            }
            Self::NotARealDirectory { component } => write!(
                f,
                "part {component} of the vault path is a symlink or not a directory \
                 (vault paths must be fully resolved)"
            ),
            Self::UnsafeParent { component } => write!(
                f,
                "part {component} of the vault path is owned by another user or writable \
                 by group or others"
            ),
            Self::NotOwned => write!(f, "the vault directory is owned by another user"),
            Self::BadMode(mode) => {
                write!(f, "the vault directory has mode {mode:o}, need 700")
            }
            Self::IsHome => write!(f, "the vault directory cannot be your home directory"),
            Self::InsideCrystalDir => write!(
                f,
                "the vault directory cannot be in Crystal's config or state directory"
            ),
            Self::ContainsCrystalDir => write!(
                f,
                "the vault directory cannot contain Crystal's config or state directory"
            ),
            Self::Version(e) => e.fmt(f),
            Self::Open(errno) => write!(f, "cannot open the vault directory ({errno})"),
            Self::Stat(errno) => write!(f, "cannot check the vault directory ({errno})"),
        }
    }
}

impl std::error::Error for OpenVaultError {}

/// A vault directory that passed every check. Its fd is the only handle
/// Crystal uses afterwards (and, from Phase 6, a Landlock rule fd).
pub struct VaultDir {
    fd: OwnedFd,
}

impl VaultDir {
    /// Opens `path` as a vault directory, or refuses it:
    ///
    /// 1. `path` is absolute and has only normal components.
    /// 2. It is opened from `/` one component at a time, each with
    ///    `O_NOFOLLOW`: a symlink anywhere is refused.
    /// 3. Every directory on the way is owned by root or by us and is not
    ///    group- or world-writable, so nobody else can swap what is below.
    /// 4. No directory on the way, nor the vault itself, is the config or
    ///    state directory; the vault is not HOME and contains neither.
    /// 5. The vault directory is ours, mode 0700, with a valid `VERSION`.
    pub fn open_checked(
        path: &OsStr,
        home: &OsStr,
        config: &ConfigDir,
        state: &StateDir,
    ) -> Result<Self, OpenVaultError> {
        let our_uid = geteuid().as_raw();
        let dir = walk(path, config, state)?;
        let root_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;

        let vault_id = dir_id(&dir)?;
        let home_id = {
            let home =
                openat(CWD, home, root_flags, Mode::empty()).map_err(OpenVaultError::Open)?;
            dir_id(&home)?
        };
        if vault_id == home_id {
            return Err(OpenVaultError::IsHome);
        }
        for crystal in [&config.fd, &state.fd] {
            if ancestors(crystal)?.contains(&vault_id) {
                return Err(OpenVaultError::ContainsCrystalDir);
            }
        }

        let stat = fstat(&dir).map_err(OpenVaultError::Stat)?;
        if stat.st_uid != our_uid {
            return Err(OpenVaultError::NotOwned);
        }
        let mode = stat.st_mode & 0o7777;
        if mode != 0o700 {
            return Err(OpenVaultError::BadMode(mode));
        }
        read_version(&dir).map_err(OpenVaultError::Version)?;
        Ok(Self { fd: dir })
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }
}

/// For `crystal init`: opens `path`, the directory a new vault will be created
/// in, with the same walk as a vault (`open_checked` steps 1 to 4 for the
/// directories on the way). The directory itself must also be a safe
/// parent: owned by root or us and not writable by group or others, so
/// nobody else can swap the new vault directory once it exists.
pub fn open_parent_checked(
    path: &OsStr,
    config: &ConfigDir,
    state: &StateDir,
) -> Result<OwnedFd, OpenVaultError> {
    let dir = walk(path, config, state)?;
    let stat = fstat(&dir).map_err(OpenVaultError::Stat)?;
    if !is_safe_parent(
        stat.st_uid,
        stat.st_mode,
        Owners::UsOrRoot,
        geteuid().as_raw(),
    ) {
        let component = components(path.as_bytes())?.len();
        return Err(OpenVaultError::UnsafeParent { component });
    }
    Ok(dir)
}

/// Opens `path` from `/`, one component at a time with `O_NOFOLLOW`. Every
/// directory descended from must be a safe parent, and no directory
/// reached may be the config or state directory.
fn walk(path: &OsStr, config: &ConfigDir, state: &StateDir) -> Result<OwnedFd, OpenVaultError> {
    let components = components(path.as_bytes())?;
    let our_uid = geteuid().as_raw();
    let crystal_dirs = [dir_id(&config.fd)?, dir_id(&state.fd)?];

    let root_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir = openat(CWD, "/", root_flags, Mode::empty()).map_err(OpenVaultError::Open)?;
    for (index, name) in components.iter().enumerate() {
        let component = index + 1;
        let stat = fstat(&dir).map_err(OpenVaultError::Stat)?;
        // The directory we are about to descend from (index 0 is `/`).
        if !is_safe_parent(stat.st_uid, stat.st_mode, Owners::UsOrRoot, our_uid) {
            return Err(OpenVaultError::UnsafeParent { component: index });
        }
        dir = openat(&dir, OsStr::from_bytes(name), OPEN_DIR_FLAGS, Mode::empty()).map_err(
            |errno| match errno {
                Errno::NOENT => OpenVaultError::Missing { component },
                Errno::NOTDIR | Errno::LOOP => OpenVaultError::NotARealDirectory { component },
                other => OpenVaultError::Open(other),
            },
        )?;
        if crystal_dirs.contains(&dir_id(&dir)?) {
            return Err(OpenVaultError::InsideCrystalDir);
        }
    }
    Ok(dir)
}

/// The components of an absolute path, checked on the raw bytes:
/// `Path::components` would silently drop `.` and repeated slashes, which
/// would make two different strings name the same checked path.
pub fn components(path: &[u8]) -> Result<Vec<&[u8]>, OpenVaultError> {
    if path.len() >= MAX_PATH || path.contains(&0) {
        return Err(OpenVaultError::BadPath);
    }
    let rest = path.strip_prefix(b"/").ok_or(OpenVaultError::BadPath)?;
    if rest.is_empty() {
        // `/` itself: never ours, and never a vault.
        return Err(OpenVaultError::BadPath);
    }
    let parts: Vec<&[u8]> = rest.split(|b| *b == b'/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == b"." || *p == b"..")
    {
        return Err(OpenVaultError::BadPath);
    }
    Ok(parts)
}

fn dir_id(fd: &OwnedFd) -> Result<DirId, OpenVaultError> {
    let stat = fstat(fd).map_err(OpenVaultError::Stat)?;
    Ok((stat.st_dev, stat.st_ino))
}

/// The identities of every directory above `fd`, up to and including `/`,
/// found by opening `..` repeatedly (which crosses mount points upwards).
/// `/` is reached when `..` is the directory itself.
fn ancestors(fd: &OwnedFd) -> Result<Vec<DirId>, OpenVaultError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut found = Vec::new();
    let mut current = openat(fd, "..", flags, Mode::empty()).map_err(OpenVaultError::Open)?;
    let mut previous = dir_id(fd)?;
    for _ in 0..MAX_DEPTH {
        let id = dir_id(&current)?;
        if id == previous {
            return Ok(found);
        }
        found.push(id);
        previous = id;
        current = openat(&current, "..", flags, Mode::empty()).map_err(OpenVaultError::Open)?;
    }
    Err(OpenVaultError::BadPath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    /// A test world under target/: a fake HOME with the config and state
    /// directories created as Crystal creates them.
    struct World {
        base: PathBuf,
        home: PathBuf,
        config: ConfigDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-vault-dir")
                .join(test);
            let _ = fs::remove_dir_all(&base);
            let home = base.join("home");
            fs::create_dir_all(&home).unwrap();
            set_mode(&base, 0o755);
            set_mode(&home, 0o700);
            let config = ConfigDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self {
                base,
                home,
                config,
                state,
            }
        }

        /// A valid vault at `path`: mode 0700 and a VERSION file.
        fn make_vault(&self, path: &Path) {
            fs::create_dir_all(path).unwrap();
            set_mode(path, 0o700);
            fs::write(path.join("VERSION"), b"1\n").unwrap();
        }

        fn open(&self, path: &Path) -> Result<VaultDir, OpenVaultError> {
            self.open_raw(path.as_os_str())
        }

        fn open_raw(&self, path: &OsStr) -> Result<VaultDir, OpenVaultError> {
            VaultDir::open_checked(path, self.home.as_os_str(), &self.config, &self.state)
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The base directory itself must pass the walk, or every test below
    /// fails for the wrong reason. Checked once, with a clear message.
    fn assert_test_location_is_safe(world: &World) {
        let probe = world.base.join("probe");
        world.make_vault(&probe);
        if let Err(e) = world.open(&probe) {
            panic!(
                "the test directory's own path fails the vault checks ({e}); the repository \
                 must be under directories owned by root or you, not group- or world-writable"
            );
        }
    }

    // Acceptance: a fresh ~/Vaults/x and a directory elsewhere, with the
    // right mode.
    #[test]
    fn accepts_a_private_vault() {
        let world = World::new("accept");
        assert_test_location_is_safe(&world);
        let in_home = world.home.join("Vaults/x");
        world.make_vault(&in_home);
        set_mode(&world.home.join("Vaults"), 0o700);
        let vault = world.open(&in_home).unwrap();
        // The fd is the checked directory itself.
        assert_eq!(read_version(vault.fd()), Ok(()));

        let elsewhere = world.base.join("synced/notes");
        world.make_vault(&elsewhere);
        set_mode(&world.base.join("synced"), 0o755);
        assert!(world.open(&elsewhere).is_ok());
    }

    #[test]
    fn refuses_bad_spellings() {
        let world = World::new("spelling");
        let vault = world.base.join("v");
        world.make_vault(&vault);
        let good = vault.as_os_str().as_bytes().to_vec();
        let mut bad: Vec<Vec<u8>> = vec![
            b"relative/path".to_vec(),
            b"".to_vec(),
            b"/".to_vec(),
            [&good[..], b"/"].concat(),
            [&good[..], b"/."].concat(),
            [&good[..], b"/../v"].concat(),
            [&good[..], b"/./"].concat(),
            [b"/".as_slice(), &good[..]].concat(),
            [&good[..], b"\0"].concat(),
            [b"/".as_slice(), &[b'a'; MAX_PATH]].concat(),
        ];
        // `.` inside: /base/./v
        let dot = world.base.join(".").join("v");
        bad.push(dot.as_os_str().as_bytes().to_vec());
        for path in bad {
            let err = world.open_raw(OsStr::from_bytes(&path)).err();
            assert_eq!(
                err,
                Some(OpenVaultError::BadPath),
                "{:?}",
                String::from_utf8_lossy(&path)
            );
        }
        assert!(world.open(&vault).is_ok());
    }

    // A symlink at the last component and in the middle are both refused.
    #[test]
    fn refuses_symlinks_anywhere() {
        let world = World::new("symlink");
        let real = world.base.join("real/v");
        world.make_vault(&real);
        set_mode(&world.base.join("real"), 0o755);

        symlink(&real, world.base.join("link")).unwrap();
        let err = world.open(&world.base.join("link")).err();
        assert!(
            matches!(err, Some(OpenVaultError::NotARealDirectory { .. })),
            "{err:?}"
        );

        symlink(world.base.join("real"), world.base.join("mid")).unwrap();
        let err = world.open(&world.base.join("mid/v")).err();
        assert!(
            matches!(err, Some(OpenVaultError::NotARealDirectory { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn refuses_writable_parents() {
        let world = World::new("writable");
        let vault = world.base.join("parent/v");
        world.make_vault(&vault);
        for mode in [0o775, 0o757, 0o777, 0o1777, 0o770] {
            set_mode(&world.base.join("parent"), mode);
            let err = world.open(&vault).err();
            assert!(
                matches!(err, Some(OpenVaultError::UnsafeParent { .. })),
                "{mode:o}: {err:?}"
            );
        }
        set_mode(&world.base.join("parent"), 0o755);
        assert!(world.open(&vault).is_ok());

        // /tmp: root-owned, sticky and world-writable. Refused at /tmp
        // itself, before anything below it is looked at.
        let err = world.open(Path::new("/tmp/crystal-no-such-vault")).err();
        assert_eq!(err, Some(OpenVaultError::UnsafeParent { component: 1 }));
    }

    // Another user's directory cannot be created in a test without root,
    // so the rule is checked directly; root-owned parents (/usr) are real.
    #[test]
    fn parent_owner_rule() {
        let us = 1000;
        assert!(is_safe_parent(us, 0o40755, Owners::UsOrRoot, us));
        assert!(is_safe_parent(0, 0o40755, Owners::UsOrRoot, us));
        assert!(!is_safe_parent(1001, 0o40755, Owners::UsOrRoot, us));
        assert!(!is_safe_parent(0, 0o40755, Owners::Us, us));
        assert!(!is_safe_parent(us, 0o40775, Owners::UsOrRoot, us));
        assert!(!is_safe_parent(0, 0o41777, Owners::UsOrRoot, us));
    }

    #[test]
    fn refuses_vault_not_ours_or_wrong_mode() {
        let world = World::new("mode");
        if !geteuid().is_root() {
            // /usr: every component root-owned and safe, but not ours.
            assert_eq!(
                world.open(Path::new("/usr")).err(),
                Some(OpenVaultError::NotOwned)
            );
        }
        let vault = world.base.join("v");
        world.make_vault(&vault);
        for mode in [0o750, 0o755, 0o711, 0o500] {
            set_mode(&vault, mode);
            assert_eq!(
                world.open(&vault).err(),
                Some(OpenVaultError::BadMode(mode))
            );
        }
    }

    // ~/.ssh: private and ours, but no VERSION, so not a vault.
    #[test]
    fn refuses_directory_without_version() {
        let world = World::new("no_version");
        let ssh = world.home.join(".ssh");
        fs::create_dir(&ssh).unwrap();
        set_mode(&ssh, 0o700);
        let err = world.open(&ssh).err();
        assert_eq!(err, Some(OpenVaultError::Version(VersionError::Missing)));
    }

    // HOME, the config and state directories, something inside them, and
    // an ancestor of the state directory: each refused even with mode 0700
    // and a VERSION file, so only the identity rule can refuse it.
    #[test]
    fn refuses_home_and_crystal_directories() {
        let world = World::new("identity");
        let cases: [(PathBuf, OpenVaultError); 6] = [
            (world.home.clone(), OpenVaultError::IsHome),
            (
                world.home.join(".config/crystal"),
                OpenVaultError::InsideCrystalDir,
            ),
            (
                world.home.join(".local/state/crystal"),
                OpenVaultError::InsideCrystalDir,
            ),
            (
                world.home.join(".config/crystal/v"),
                OpenVaultError::InsideCrystalDir,
            ),
            (
                world.home.join(".local/state/crystal/v"),
                OpenVaultError::InsideCrystalDir,
            ),
            (
                world.home.join(".local"),
                OpenVaultError::ContainsCrystalDir,
            ),
        ];
        for (path, expected) in cases {
            world.make_vault(&path);
            assert_eq!(world.open(&path).err(), Some(expected), "{path:?}");
        }
        let config_parent = world.home.join(".config");
        world.make_vault(&config_parent);
        assert_eq!(
            world.open(&config_parent).err(),
            Some(OpenVaultError::ContainsCrystalDir)
        );
    }

    // Acceptance: identity is compared by (dev, ino), not by string. HOME
    // given through a symlink (as /home is on Fedora Atomic) still matches
    // the real directory the vault path names.
    #[test]
    fn identity_is_by_inode_not_string() {
        let world = World::new("inode");
        world.make_vault(&world.home);
        let alias = world.base.join("home-alias");
        symlink(&world.home, &alias).unwrap();
        let err = VaultDir::open_checked(
            world.home.as_os_str(),
            alias.as_os_str(),
            &world.config,
            &world.state,
        )
        .err();
        assert_eq!(err, Some(OpenVaultError::IsHome));
    }

    #[test]
    fn errors_never_show_the_path() {
        let world = World::new("message");
        let secret = world.base.join("hunter2");
        for err in [
            world.open(&secret).err().unwrap(),
            world.open_raw(OsStr::new("hunter2")).err().unwrap(),
        ] {
            for shown in [err.to_string(), format!("{err:?}")] {
                assert!(!shown.contains("hunter2"), "{shown}");
            }
        }
    }
}
