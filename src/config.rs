//! `~/.config/crystal/config.age` (docs/design/storage.md, "Config file"):
//! which vaults this device knows, by name, and a few settings.
//!
//! Format 1, postcard, then Padmé, then age to a scrypt passphrase:
//!
//! ```text
//! format: u16 | generation: u64 | lock_after: Option<u32> | vaults: [Entry]
//! Entry = name: str | path: str | vault_id: [u8; 32] | lock_after: Option<u32>
//! ```
//!
//! The config is authenticated only as strongly as its recipient, so
//! nothing in it is trusted for a security decision: names and paths are
//! validated when decoding, and every path is checked again as
//! attacker-written when a vault is opened (`vault::vault_dir`). Only
//! `crystal init`, `crystal add` and `crystal remove` write it, and then exit.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::cli::MAX_LOCK_AFTER_MINUTES;
use crate::crypto::{self, Ciphertext, CryptoError, Passphrase, VaultIdentity};
use crate::model::VaultId;
use crate::vault::atomic::{FileName, Payload, WriteError, atomic_write};
use crate::vault::format::{
    self as vault_format, IDENTITY_FILE, MAX_IDENTITY_FILE, VAULT_FILE, VaultError,
};
use crate::vault::generation::{self, Counter, GenError, Seen};
use crate::vault::padme::{self, PadError};
use crate::vault::public_file::{ReadError, read_bounded};
use crate::vault::vault_dir::{VaultDir, components};
use crate::vault::{ConfigDir, StateDir, VaultName};

pub const CONFIG_FILE: &str = "config.age";

/// The config format this build reads and writes.
pub const CONFIG_FORMAT: u16 = 1;
/// More vaults than anyone keeps; bounds what a crafted config can make
/// Crystal hold.
pub const MAX_VAULTS: usize = 256;
/// PATH_MAX without the terminating NUL.
const MAX_PATH_BYTES: usize = 4095;

/// One registered vault. `vault_id` is pinned when the vault is added
/// (`crystal init`, `crystal add`), so a normal session never writes the config.
pub struct VaultEntry {
    pub name: VaultName,
    /// Absolute and fully resolved (no symlinks; checked again at open).
    pub path: String,
    pub vault_id: VaultId,
    /// Minutes; overrides the config's default.
    pub lock_after: Option<u32>,
}

/// The decrypted config. No Debug: it lists where the vaults are.
pub struct Config {
    pub generation: u64,
    /// Minutes; `None` means Crystal's default (5).
    pub lock_after: Option<u32>,
    pub vaults: Vec<VaultEntry>,
}

#[derive(Serialize, Deserialize)]
struct WireEntry<'a> {
    name: &'a str,
    path: &'a str,
    vault_id: [u8; 32],
    lock_after: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct WireConfig<'a> {
    generation: u64,
    lock_after: Option<u32>,
    #[serde(borrow)]
    vaults: Vec<WireEntry<'a>>,
}

/// Why the config was refused or could not be written. Fixed text only:
/// never a name, a path or a byte of the file.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// `config.age` is not there (it was a moment ago, or never was).
    Missing,
    Read(ReadError),
    TooLarge,
    /// The vault's `identity.age` or `vault.age` is over its size limit
    /// (`crystal add`).
    VaultFileTooLarge(&'static str),
    Crypto(CryptoError),
    Padding(PadError),
    Malformed,
    UnknownFormat,
    TooManyVaults,
    BadName,
    DuplicateName,
    BadPath,
    BadLockAfter,
    Encode,
    Generation(GenError),
    GenerationOverflow,
    Write(WriteError),
    /// `crystal add` with a name the config already has.
    NameTaken,
    /// `crystal add` of a vault (same `vault_id`) already registered under
    /// another name.
    VaultTaken,
    /// `crystal add`: the vault's `identity.age` does not open with the
    /// passphrase given. `new_device`: there was no config, so the
    /// passphrase was new; otherwise it was the config's.
    VaultPassphrase {
        new_device: bool,
    },
    /// `crystal remove` (or a run) with a name the config does not have.
    UnknownName,
    /// The vault directory has no `identity.age` or `vault.age`.
    NotAVault,
    /// The vault's files could not be opened with this passphrase.
    Vault(VaultError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "this device has no config.age"),
            Self::Read(e) => write!(f, "cannot read a Crystal file ({e})"),
            Self::TooLarge => write!(f, "config.age is too large"),
            Self::VaultFileTooLarge(file) => write!(f, "the vault's {file} is too large"),
            Self::Crypto(e) => write!(f, "config.age: {e}"),
            Self::Padding(e) => write!(f, "config.age: {e}"),
            Self::Malformed => write!(f, "config.age is malformed"),
            Self::UnknownFormat => write!(
                f,
                "config.age uses a format this version of Crystal cannot read"
            ),
            Self::TooManyVaults => {
                write!(f, "the config has more than {MAX_VAULTS} vaults")
            }
            Self::BadName => write!(f, "the config holds an invalid vault name"),
            Self::DuplicateName => write!(f, "the config holds the same vault name twice"),
            Self::BadPath => write!(
                f,
                "the config holds a vault path that is not absolute, clean UTF-8"
            ),
            Self::BadLockAfter => write!(
                f,
                "the config holds a lock time outside 1 to {MAX_LOCK_AFTER_MINUTES} minutes"
            ),
            Self::Encode => write!(f, "cannot encode the config"),
            Self::Generation(e) => e.fmt(f),
            Self::GenerationOverflow => write!(f, "the config's generation cannot grow"),
            Self::Write(e) => write!(f, "cannot write config.age: {e}"),
            Self::NameTaken => write!(f, "a vault with that name is already registered"),
            Self::VaultTaken => write!(
                f,
                "this vault is already registered on this device under another name"
            ),
            Self::VaultPassphrase { new_device: true } => write!(
                f,
                "the vault's identity.age does not open with this passphrase; enter the \
                 passphrase the vault was made with"
            ),
            Self::VaultPassphrase { new_device: false } => write!(
                f,
                "the vault's identity.age does not open with this device's passphrase. A device \
                 has one passphrase for its config and every vault on it, so this vault cannot be \
                 added here until its passphrase is changed (not implemented yet)"
            ),
            Self::UnknownName => write!(f, "no vault with that name is registered"),
            Self::NotAVault => write!(
                f,
                "the directory has no identity.age or vault.age; create a vault with crystal init"
            ),
            Self::Vault(e) => write!(f, "cannot open the vault: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn lock_after_ok(minutes: Option<u32>) -> bool {
    minutes.is_none_or(|m| (1..=MAX_LOCK_AFTER_MINUTES).contains(&m))
}

impl Config {
    /// The config of a device that has none yet.
    pub fn new() -> Self {
        Self {
            generation: 0,
            lock_after: None,
            vaults: Vec::new(),
        }
    }

    pub fn find(&self, name: &VaultName) -> Option<&VaultEntry> {
        self.vaults.iter().find(|v| v.name == *name)
    }

    /// Adds `entry`; its name and its vault must be new.
    pub fn add(&mut self, entry: VaultEntry) -> Result<(), ConfigError> {
        if self.find(&entry.name).is_some() {
            return Err(ConfigError::NameTaken);
        }
        if self.vaults.iter().any(|v| v.vault_id == entry.vault_id) {
            return Err(ConfigError::VaultTaken);
        }
        if self.vaults.len() >= MAX_VAULTS {
            return Err(ConfigError::TooManyVaults);
        }
        self.vaults.push(entry);
        Ok(())
    }

    /// Forgets `name`. The vault's files are never touched.
    pub fn remove(&mut self, name: &VaultName) -> Result<VaultEntry, ConfigError> {
        let at = self
            .vaults
            .iter()
            .position(|v| v.name == *name)
            .ok_or(ConfigError::UnknownName)?;
        Ok(self.vaults.remove(at))
    }

    fn wire(&self) -> WireConfig<'_> {
        WireConfig {
            generation: self.generation,
            lock_after: self.lock_after,
            vaults: self
                .vaults
                .iter()
                .map(|v| WireEntry {
                    name: v.name.as_str(),
                    path: &v.path,
                    vault_id: *v.vault_id.as_bytes(),
                    lock_after: v.lock_after,
                })
                .collect(),
        }
    }
}

impl WireConfig<'_> {
    /// Everything a config must satisfy, whoever wrote it.
    fn check(&self) -> Result<(), ConfigError> {
        if self.vaults.len() > MAX_VAULTS {
            return Err(ConfigError::TooManyVaults);
        }
        if !lock_after_ok(self.lock_after) {
            return Err(ConfigError::BadLockAfter);
        }
        for (i, entry) in self.vaults.iter().enumerate() {
            if VaultName::new(entry.name.into()).is_none() {
                return Err(ConfigError::BadName);
            }
            if self.vaults[..i].iter().any(|e| e.name == entry.name) {
                return Err(ConfigError::DuplicateName);
            }
            if entry.path.len() > MAX_PATH_BYTES || components(entry.path.as_bytes()).is_err() {
                return Err(ConfigError::BadPath);
            }
            if !lock_after_ok(entry.lock_after) {
                return Err(ConfigError::BadLockAfter);
            }
        }
        Ok(())
    }
}

/// The config as postcard bytes, after the same checks `decode` applies.
fn encode(config: &Config) -> Result<Vec<u8>, ConfigError> {
    let wire = config.wire();
    wire.check()?;
    postcard::to_allocvec(&(CONFIG_FORMAT, &wire)).map_err(|_| ConfigError::Encode)
}

/// The config back from postcard bytes. The format number is decoded
/// first; any broken rule or leftover byte is refused.
fn decode(bytes: &[u8]) -> Result<Config, ConfigError> {
    let (format, rest): (u16, _) =
        postcard::take_from_bytes(bytes).map_err(|_| ConfigError::Malformed)?;
    if format != CONFIG_FORMAT {
        return Err(ConfigError::UnknownFormat);
    }
    let (wire, rest): (WireConfig<'_>, _) =
        postcard::take_from_bytes(rest).map_err(|_| ConfigError::Malformed)?;
    if !rest.is_empty() {
        return Err(ConfigError::Malformed);
    }
    wire.check()?;
    let vaults = wire
        .vaults
        .iter()
        .map(|e| {
            Ok(VaultEntry {
                name: VaultName::new(e.name.into()).ok_or(ConfigError::BadName)?,
                path: e.path.to_owned(),
                vault_id: VaultId::from_bytes(e.vault_id),
                lock_after: e.lock_after,
            })
        })
        .collect::<Result<_, ConfigError>>()?;
    Ok(Config {
        generation: wire.generation,
        lock_after: wire.lock_after,
        vaults,
    })
}

/// The bytes of `config.age`: encode, pad, encrypt to the passphrase.
fn seal(config: &Config, pass: &Passphrase) -> Result<Ciphertext, ConfigError> {
    let encoded = encode(config)?;
    let padded = padme::pad(&encoded).map_err(ConfigError::Padding)?;
    crypto::encrypt_to_passphrase(&padded, pass).map_err(ConfigError::Crypto)
}

/// The config back from `config.age` bytes.
fn open(ciphertext: &[u8], pass: &Passphrase) -> Result<Config, ConfigError> {
    let padded = crypto::decrypt_with_passphrase(ciphertext, pass).map_err(ConfigError::Crypto)?;
    let encoded = padme::unpad(&padded).map_err(ConfigError::Padding)?;
    decode(encoded)
}

/// Whether this device has a config yet (decides which passphrase prompt
/// `crystal add` shows). Only the file's existence is looked at.
pub fn exists(config_dir: &ConfigDir) -> Result<bool, ConfigError> {
    // Reading one byte is enough to tell "missing" from "present".
    read_bounded(config_dir.fd(), CONFIG_FILE, 0)
        .map(|found| found.is_some())
        .map_err(ConfigError::Read)
}

/// Decrypts `config.age` and checks it against `config.gen`: an older
/// config is refused as a possible rollback. `None` if there is no config.
pub fn load(
    config_dir: &ConfigDir,
    state: &StateDir,
    pass: &Passphrase,
) -> Result<Option<(Config, Seen)>, ConfigError> {
    let Some(bytes) = read_bounded(config_dir.fd(), CONFIG_FILE, crypto::MAX_CIPHERTEXT)
        .map_err(ConfigError::Read)?
    else {
        return Ok(None);
    };
    if bytes.len() > crypto::MAX_CIPHERTEXT {
        return Err(ConfigError::TooLarge);
    }
    let config = open(&bytes, pass)?;
    let seen = generation::accept_on_open(state.fd(), Counter::Config, config.generation)
        .map_err(ConfigError::Generation)?;
    Ok(Some((config, seen)))
}

/// The config for a device without `config.age` (`crystal init`, `crystal add`).
/// `config.gen` survives a lost config, so the new config starts at the
/// stored generation: its first `store` writes one above it, never a
/// generation the counter would refuse as a rollback. A corrupt counter
/// is refused, as everywhere else.
pub fn fresh(state: &StateDir) -> Result<Config, ConfigError> {
    let stored = generation::load(state.fd(), Counter::Config).map_err(ConfigError::Generation)?;
    Ok(Config {
        generation: stored.unwrap_or(0),
        ..Config::new()
    })
}

/// Writes the config with its generation raised by one: atomic write of
/// the new `config.age`, then `config.gen` once the rename has committed.
pub fn store(
    config: &mut Config,
    config_dir: &ConfigDir,
    state: &StateDir,
    pass: &Passphrase,
) -> Result<(), ConfigError> {
    config.generation = config
        .generation
        .checked_add(1)
        .ok_or(ConfigError::GenerationOverflow)?;
    let sealed = seal(config, pass)?;
    // A fixed, valid name: FileName::new cannot refuse it.
    let name = FileName::new(CONFIG_FILE).ok_or(ConfigError::Encode)?;
    atomic_write(config_dir.fd(), &name, Payload::Ciphertext(&sealed))
        .map_err(ConfigError::Write)?;
    generation::record_save(state.fd(), Counter::Config, config.generation)
        .map_err(ConfigError::Generation)
}

/// For `crystal add`: opens a vault directory's `identity.age` with `pass`,
/// then its `vault.age`, to learn the `vault_id` to pin and the vault's
/// generation. The notes are dropped (and wiped) at once.
pub fn identify_vault(vault: &VaultDir, pass: &Passphrase) -> Result<(VaultId, u64), ConfigError> {
    // read_bounded reads one byte past `max` to show the file is longer.
    let read = |name: &'static str, max: usize| {
        let bytes = read_bounded(vault.fd(), name, max)
            .map_err(ConfigError::Read)?
            .ok_or(ConfigError::NotAVault)?;
        if bytes.len() > max {
            return Err(ConfigError::VaultFileTooLarge(name));
        }
        Ok(bytes)
    };
    let identity_file = read(IDENTITY_FILE, MAX_IDENTITY_FILE)?;
    let identity = VaultIdentity::open_with_passphrase(&identity_file, pass)
        .map_err(|e| ConfigError::Vault(VaultError::Crypto(e)))?;
    let vault_file = read(VAULT_FILE, crypto::MAX_CIPHERTEXT)?;
    let contents = vault_format::open(&vault_file, &identity).map_err(ConfigError::Vault)?;
    Ok((contents.vault_id, contents.generation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::codec::VaultContents;
    use rustix::fs::{Mode, OFlags, openat};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    fn pass(s: &str) -> Passphrase {
        Passphrase::new(Zeroizing::new(s.as_bytes().to_vec())).unwrap()
    }

    fn name(s: &str) -> VaultName {
        VaultName::new(s.into()).unwrap()
    }

    fn entry(n: &str, path: &str) -> VaultEntry {
        VaultEntry {
            name: name(n),
            path: path.to_owned(),
            vault_id: VaultId::from_bytes(
                [n.bytes()
                    .fold(0u8, |h, b| h.wrapping_mul(31).wrapping_add(b)); 32],
            ),
            lock_after: Some(10),
        }
    }

    fn sample() -> Config {
        Config {
            generation: 4,
            lock_after: Some(5),
            vaults: vec![
                entry("diary", "/var/home/u/Vaults/diary"),
                entry("work", "/srv/w"),
            ],
        }
    }

    /// A fake HOME under target/ with the config and state directories.
    struct World {
        base: PathBuf,
        home: PathBuf,
        config: ConfigDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-config")
                .join(test);
            let _ = fs::remove_dir_all(&base);
            let home = base.join("home");
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            let config = ConfigDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self {
                base,
                home,
                config,
                state,
            }
        }

        fn config_file(&self) -> PathBuf {
            self.home.join(".config/crystal/config.age")
        }

        /// A complete vault directory as `crystal init` will make it (#14).
        fn make_vault(&self, dir: &str, pw: &str, id: [u8; 32], generation: u64) -> PathBuf {
            let path = self.base.join(dir);
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
            let fd = openat(rustix::fs::CWD, &path, flags, Mode::empty()).unwrap();
            vault_format::write_version(&fd).unwrap();
            let identity = VaultIdentity::generate();
            let sealed = identity.seal_with_passphrase(&pass(pw)).unwrap();
            fs::write(path.join(IDENTITY_FILE), sealed.as_bytes()).unwrap();
            let contents = VaultContents {
                vault_id: VaultId::from_bytes(id),
                generation,
                notes: vec![],
            };
            let sealed = vault_format::seal(&contents, &identity.recipient()).unwrap();
            fs::write(path.join(VAULT_FILE), sealed.as_bytes()).unwrap();
            path
        }

        fn open_vault(&self, path: &Path) -> VaultDir {
            VaultDir::open_checked(
                path.as_os_str(),
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
            .unwrap()
        }
    }

    fn same(a: &Config, b: &Config) -> bool {
        a.generation == b.generation
            && a.lock_after == b.lock_after
            && a.vaults.len() == b.vaults.len()
            && a.vaults.iter().zip(&b.vaults).all(|(x, y)| {
                x.name == y.name
                    && x.path == y.path
                    && x.vault_id == y.vault_id
                    && x.lock_after == y.lock_after
            })
    }

    #[test]
    fn round_trip() {
        let config = sample();
        let back = open(seal(&config, &pass("pw")).unwrap().as_bytes(), &pass("pw")).unwrap();
        assert!(same(&back, &config));
        let empty = Config::new();
        assert!(same(&decode(&encode(&empty).unwrap()).unwrap(), &empty));
    }

    #[test]
    fn store_then_load_raises_generation() {
        let world = World::new("store_load");
        let mut config = sample();
        store(&mut config, &world.config, &world.state, &pass("pw")).unwrap();
        assert_eq!(config.generation, 5);
        let mode = fs::metadata(world.config_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o600);
        let (loaded, seen) = load(&world.config, &world.state, &pass("pw"))
            .unwrap()
            .unwrap();
        assert!(same(&loaded, &config));
        assert_eq!(seen, Seen::Known);
        assert!(exists(&world.config).unwrap());
    }

    // A lost config.age with config.gen still there: the new config starts
    // above the counter, so it is stored and loads again. A corrupt
    // counter is refused, never reset.
    #[test]
    fn fresh_config_starts_above_the_stored_counter() {
        let world = World::new("fresh");
        assert_eq!(
            fresh(&world.state).unwrap().generation,
            0,
            "first device use"
        );

        let mut config = sample();
        store(&mut config, &world.config, &world.state, &pass("pw")).unwrap();
        fs::remove_file(world.config_file()).unwrap();

        let mut again = fresh(&world.state).unwrap();
        assert_eq!(again.generation, 5);
        assert!(again.vaults.is_empty());
        store(&mut again, &world.config, &world.state, &pass("pw")).unwrap();
        assert_eq!(again.generation, 6);
        assert!(load(&world.config, &world.state, &pass("pw")).is_ok());

        let counter = world.home.join(".local/state/crystal/config.gen");
        fs::write(&counter, b"x\n").unwrap();
        assert_eq!(
            fresh(&world.state).err(),
            Some(ConfigError::Generation(GenError::Corrupt {
                what: "config"
            }))
        );
    }

    #[test]
    fn missing_config_is_none() {
        let world = World::new("missing");
        assert!(!exists(&world.config).unwrap());
        assert!(
            load(&world.config, &world.state, &pass("pw"))
                .unwrap()
                .is_none()
        );
    }

    // Acceptance: the config tamper cases. A config for another
    // passphrase, truncated, bit-flipped or older than config.gen is
    // refused.
    #[test]
    fn tampered_configs_are_refused() {
        let world = World::new("tamper");
        let mut config = sample();
        store(&mut config, &world.config, &world.state, &pass("pw")).unwrap();
        let good = fs::read(world.config_file()).unwrap();
        let load_with = |bytes: &[u8], pw: &str| {
            fs::write(world.config_file(), bytes).unwrap();
            load(&world.config, &world.state, &pass(pw)).err()
        };

        let foreign = seal(&sample(), &pass("someone else")).unwrap();
        assert_eq!(
            load_with(foreign.as_bytes(), "pw"),
            Some(ConfigError::Crypto(CryptoError::WrongPassphrase))
        );
        assert!(
            load_with(&good[..good.len() - 1], "pw").is_some(),
            "truncated"
        );
        let mut flipped = good.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert_eq!(
            load_with(&flipped, "pw"),
            Some(ConfigError::Crypto(CryptoError::Corrupt))
        );

        // An older, validly encrypted config: generation 5 is stored, 4 is
        // offered.
        let mut older = sample();
        older.generation = 4;
        let older = seal(&older, &pass("pw")).unwrap();
        assert_eq!(
            load_with(older.as_bytes(), "pw"),
            Some(ConfigError::Generation(GenError::Rollback {
                what: "config",
                seen: 4,
                stored: 5
            }))
        );
        assert!(
            load_with(&good, "pw").is_none(),
            "the real config still loads"
        );
    }

    /// Bytes for a wire config, without encode's checks: what an attacker
    /// holding the recipient could encrypt.
    fn crafted(wire: &WireConfig<'_>) -> Vec<u8> {
        postcard::to_allocvec(&(CONFIG_FORMAT, wire)).unwrap()
    }

    fn wire_entry<'a>(name: &'a str, path: &'a str) -> WireEntry<'a> {
        WireEntry {
            name,
            path,
            vault_id: [1; 32],
            lock_after: None,
        }
    }

    // Attacker-written configs: every rule is enforced at decode.
    #[test]
    fn crafted_configs_are_refused() {
        let cases: Vec<(Vec<WireEntry<'_>>, Option<u32>, ConfigError)> = vec![
            (
                vec![wire_entry("a", "/x"), wire_entry("a", "/y")],
                None,
                ConfigError::DuplicateName,
            ),
            (vec![wire_entry("../a", "/x")], None, ConfigError::BadName),
            (vec![wire_entry("-a", "/x")], None, ConfigError::BadName),
            (
                vec![wire_entry("a", "relative")],
                None,
                ConfigError::BadPath,
            ),
            (vec![wire_entry("a", "/x/../y")], None, ConfigError::BadPath),
            (vec![wire_entry("a", "/x//y")], None, ConfigError::BadPath),
            (vec![wire_entry("a", "/")], None, ConfigError::BadPath),
            (vec![], Some(0), ConfigError::BadLockAfter),
            (
                vec![],
                Some(MAX_LOCK_AFTER_MINUTES + 1),
                ConfigError::BadLockAfter,
            ),
        ];
        for (vaults, lock_after, expected) in cases {
            let wire = WireConfig {
                generation: 1,
                lock_after,
                vaults,
            };
            assert_eq!(decode(&crafted(&wire)).err(), Some(expected));
        }
        let many: Vec<String> = (0..=MAX_VAULTS).map(|i| format!("v{i}")).collect();
        let wire = WireConfig {
            generation: 1,
            lock_after: None,
            vaults: many.iter().map(|n| wire_entry(n, "/x")).collect(),
        };
        assert_eq!(
            decode(&crafted(&wire)).err(),
            Some(ConfigError::TooManyVaults)
        );

        let mut bytes = encode(&sample()).unwrap();
        bytes.push(0);
        assert_eq!(decode(&bytes).err(), Some(ConfigError::Malformed));
        bytes[0] = 2;
        assert_eq!(decode(&bytes).err(), Some(ConfigError::UnknownFormat));
    }

    #[test]
    fn add_and_remove() {
        let mut config = Config::new();
        config.add(entry("diary", "/a")).unwrap();
        assert_eq!(
            config.add(entry("diary", "/b")).err(),
            Some(ConfigError::NameTaken)
        );
        assert_eq!(
            config.remove(&name("nope")).err(),
            Some(ConfigError::UnknownName)
        );
        // U3: the same vault (vault_id) under a second name is refused.
        let same_vault = VaultEntry {
            name: name("alias"),
            ..entry("diary", "/a")
        };
        assert_eq!(config.add(same_vault).err(), Some(ConfigError::VaultTaken));
        assert_eq!(config.vaults.len(), 1);
        let removed = config.remove(&name("diary")).unwrap();
        assert_eq!(removed.path, "/a");
        assert!(config.vaults.is_empty());
    }

    // `crystal add`'s core: a real vault directory is opened with the passphrase,
    // and its vault_id and generation are read to be pinned.
    #[test]
    fn identify_vault_reads_the_vault_id() {
        let world = World::new("identify");
        let path = world.make_vault("v", "pw", [5; 32], 9);
        let vault = world.open_vault(&path);
        assert_eq!(
            identify_vault(&vault, &pass("pw")).unwrap(),
            (VaultId::from_bytes([5; 32]), 9)
        );
        assert_eq!(
            identify_vault(&vault, &pass("other")).err(),
            Some(ConfigError::Vault(VaultError::Crypto(
                CryptoError::WrongPassphrase
            )))
        );
        fs::remove_file(path.join(VAULT_FILE)).unwrap();
        assert_eq!(
            identify_vault(&vault, &pass("pw")).err(),
            Some(ConfigError::NotAVault)
        );
        // An identity.age over its limit is refused as too large, not read
        // truncated and reported as damaged.
        fs::write(path.join(IDENTITY_FILE), vec![b'x'; MAX_IDENTITY_FILE + 1]).unwrap();
        assert_eq!(
            identify_vault(&vault, &pass("pw")).err(),
            Some(ConfigError::VaultFileTooLarge(IDENTITY_FILE))
        );
    }

    #[test]
    fn errors_never_show_names_or_paths() {
        for err in [
            ConfigError::NameTaken,
            ConfigError::UnknownName,
            ConfigError::BadPath,
            ConfigError::DuplicateName,
            ConfigError::VaultFileTooLarge(IDENTITY_FILE),
            ConfigError::VaultTaken,
            ConfigError::VaultPassphrase { new_device: true },
            ConfigError::VaultPassphrase { new_device: false },
        ] {
            let shown = err.to_string();
            assert!(!shown.contains("diary") && !shown.contains('/'), "{shown}");
        }
    }
}
