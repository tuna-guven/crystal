//! `crystal passwd NAME` (decision 0065): a new passphrase and, with it,
//! a new vault identity, so a copy of the old `identity.age` (in a backup,
//! on another device) opens nothing saved afterwards.
//!
//! Three writes, ordered so that a crash or a failure after any of them
//! leaves a vault that opens with the old or the new passphrase:
//!
//! 1. `vault.age` sealed to the old **and** the new recipient
//!    (generation + 1): either identity opens it.
//! 2. `identity.age`: the new identity, sealed to the new passphrase.
//! 3. `vault.age` sealed to the new recipient alone (generation + 2).
//!
//! The rollback counter follows each `vault.age`. Nothing is merged: a
//! `vault.age` that changed on disk since it was read is never replaced.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "wired to the command in step K1b")
)]

use std::fmt;

use rustix::io::Errno;

use crate::crypto::{CryptoError, Passphrase, VaultIdentity, VaultRecipient};
use crate::vault::StateDir;
use crate::vault::atomic::{FileName, Payload, Step, WriteError, atomic_write};
use crate::vault::codec::{FORMAT, VaultContents};
use crate::vault::format::{self, IDENTITY_FILE, VAULT_FILE, VaultError};
use crate::vault::generation::{self, Counter, GenError};
use crate::vault::public_file::{FileStamp, stamp};
use crate::vault::vault_dir::VaultDir;

/// What a failure stopped on. Fixed variants only: never a key or text.
#[derive(Debug, PartialEq, Eq)]
pub enum Cause {
    NewIdentity(CryptoError),
    Seal(VaultError),
    Stamp(Errno),
    /// `vault.age` is not the file that was read (Syncthing or a second
    /// Crystal replaced it): it is never overwritten (decision 0045).
    Changed,
    GenerationExhausted,
    Version(WriteError),
    Write(WriteError),
    Counter(GenError),
}

/// Which passphrase opens the vault after a failure, so the message can
/// say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reached {
    /// `identity.age` was not replaced: the old passphrase opens the vault.
    Old,
    /// `identity.age` was renamed into place, but the directory was not
    /// synced: the new passphrase opens the vault, and after a power loss
    /// the old one may. `vault.age` opens with both identities.
    NewUnconfirmed,
    /// The new passphrase opens the vault, but `vault.age` still opens with
    /// the old identity too, until the next save or `crystal passwd`.
    NewWithOldKey,
    /// Everything was written; only the rollback counter is behind, which
    /// the next unlock catches up.
    Done,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PasswdError {
    pub cause: Cause,
    pub reached: Reached,
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NewIdentity(e) => write!(f, "cannot make the new vault key: {e}"),
            Self::Seal(e) => e.fmt(f),
            Self::Stamp(errno) => write!(f, "cannot check vault.age ({errno})"),
            Self::Changed => write!(
                f,
                "vault.age changed on disk since it was read (another device or Crystal saved it)"
            ),
            Self::GenerationExhausted => write!(f, "the vault's generation counter is exhausted"),
            Self::Version(e) => write!(f, "cannot write VERSION: {e}"),
            Self::Write(e) => write!(f, "cannot write a vault file: {e}"),
            Self::Counter(e) => e.fmt(f),
        }
    }
}

impl fmt::Display for PasswdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.reached {
            Reached::Old => "the passphrase was not changed: the old one still opens the vault",
            Reached::NewUnconfirmed => {
                "the new passphrase opens the vault, but identity.age may not be on disk yet: \
                 after a power loss the old passphrase may open it instead"
            }
            Reached::NewWithOldKey => {
                "the new passphrase opens the vault, but vault.age still opens with the old \
                 key too: run crystal passwd again"
            }
            Reached::Done => {
                "the passphrase and the vault key were changed; the next unlock updates the \
                 rollback counter"
            }
        };
        write!(f, "{}; {state}", self.cause)
    }
}

impl std::error::Error for PasswdError {}

/// A passphrase change in progress: the unlocked vault and its new
/// identity. No Debug: it holds the notes and both keys.
pub struct Rekey<'a> {
    vault_dir: &'a mut VaultDir,
    state: &'a StateDir,
    contents: VaultContents,
    old: VaultRecipient,
    /// The `vault.age` on disk that `contents` came from, or that the last
    /// step wrote.
    stamp: FileStamp,
    identity: VaultIdentity,
    new: VaultRecipient,
}

impl<'a> Rekey<'a> {
    /// From an unlocked vault (`unlock::unlock`, after any `restore`
    /// question): makes the new identity. Writes nothing.
    pub fn new(
        vault_dir: &'a mut VaultDir,
        state: &'a StateDir,
        contents: VaultContents,
        old: VaultRecipient,
        stamp: FileStamp,
    ) -> Result<Self, PasswdError> {
        let identity = VaultIdentity::generate().map_err(|e| PasswdError {
            cause: Cause::NewIdentity(e),
            reached: Reached::Old,
        })?;
        let new = identity.vault_recipient();
        Ok(Self {
            vault_dir,
            state,
            contents,
            old,
            stamp,
            identity,
            new,
        })
    }

    /// All three steps.
    pub fn run(mut self, pass: &Passphrase) -> Result<(), PasswdError> {
        self.seal_to_both()?;
        self.seal_identity(pass)?;
        self.seal_to_new()
    }

    /// Step 1: `vault.age` to the old and the new recipient.
    fn seal_to_both(&mut self) -> Result<(), PasswdError> {
        let at = |cause| PasswdError {
            cause,
            reached: Reached::Old,
        };
        self.next_generation().map_err(at)?;
        let sealed = format::seal_moving(&self.contents, &self.old, &self.new)
            .map_err(|e| at(Cause::Seal(e)))?;
        self.write_vault(&sealed).map_err(at)?;
        self.record().map_err(at)
    }

    /// Step 2: the new identity in `identity.age`, sealed to `pass`.
    fn seal_identity(&mut self, pass: &Passphrase) -> Result<(), PasswdError> {
        let sealed = self
            .identity
            .seal_with_passphrase(pass)
            .map_err(|e| PasswdError {
                cause: Cause::NewIdentity(e),
                reached: Reached::Old,
            })?;
        let name = fixed_name(IDENTITY_FILE).map_err(|cause| PasswdError {
            cause,
            reached: Reached::Old,
        })?;
        atomic_write(self.vault_dir.fd(), &name, Payload::Ciphertext(&sealed)).map_err(|e| {
            PasswdError {
                // After the rename, only the directory sync failed.
                reached: if e.step == Step::DirFsync {
                    Reached::NewUnconfirmed
                } else {
                    Reached::Old
                },
                cause: Cause::Write(e),
            }
        })
    }

    /// Step 3: `vault.age` to the new recipient alone.
    fn seal_to_new(&mut self) -> Result<(), PasswdError> {
        let at = |cause| PasswdError {
            cause,
            reached: Reached::NewWithOldKey,
        };
        self.next_generation().map_err(at)?;
        let sealed = format::seal(&self.contents, &self.new).map_err(|e| at(Cause::Seal(e)))?;
        self.write_vault(&sealed).map_err(at)?;
        self.record().map_err(|cause| PasswdError {
            cause,
            reached: Reached::Done,
        })
    }

    fn next_generation(&mut self) -> Result<(), Cause> {
        self.contents.generation = self
            .contents
            .generation
            .checked_add(1)
            .ok_or(Cause::GenerationExhausted)?;
        Ok(())
    }

    /// Replaces `vault.age` with `sealed`, unless it changed on disk since
    /// it was read or last written here; writes `VERSION` first for a
    /// format-1 vault. A failed directory sync counts as a failure: the
    /// next step must not run on a rename that may not last.
    fn write_vault(&mut self, sealed: &crate::crypto::Ciphertext) -> Result<(), Cause> {
        let fd = self.vault_dir.fd();
        if stamp(fd, VAULT_FILE).map_err(Cause::Stamp)? != Some(self.stamp) {
            return Err(Cause::Changed);
        }
        if self.vault_dir.version() < FORMAT {
            format::write_version(fd).map_err(Cause::Version)?;
            self.vault_dir.set_version(FORMAT);
        }
        let fd = self.vault_dir.fd();
        atomic_write(fd, &fixed_name(VAULT_FILE)?, Payload::Ciphertext(sealed))
            .map_err(Cause::Write)?;
        self.stamp = stamp(fd, VAULT_FILE)
            .map_err(Cause::Stamp)?
            .ok_or(Cause::Stamp(Errno::NOENT))?;
        Ok(())
    }

    fn record(&self) -> Result<(), Cause> {
        let counter = Counter::Vault(self.contents.vault_id);
        generation::record_save(self.state.fd(), counter, self.contents.generation)
            .map_err(Cause::Counter)
    }
}

/// `identity.age` and `vault.age` are fixed, valid names: FileName::new
/// cannot refuse them.
fn fixed_name(file: &'static str) -> Result<FileName, Cause> {
    FileName::new(file).ok_or(Cause::Write(WriteError {
        step: Step::Open,
        errno: Errno::INVAL,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init;
    use crate::unlock::{self, Unlocked};
    use crate::vault::atomic::FailAt;
    use crate::vault::{DataDir, VaultName};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    const OLD: &str = "pw";
    const NEW: &str = "a-new-passphrase";

    fn pass(s: &str) -> Passphrase {
        Passphrase::new(Zeroizing::new(s.as_bytes().to_vec())).unwrap()
    }

    fn name() -> VaultName {
        VaultName::new("diary".into()).unwrap()
    }

    /// A private HOME with one vault, `diary`, made by `crystal init`'s
    /// own code with passphrase `OLD`.
    struct World {
        home: PathBuf,
        data: DataDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let home = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-passwd")
                .join(test);
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            let data = DataDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            let plan = init::plan(&data, &name()).unwrap();
            init::create(plan, &name(), &pass(OLD), &data, &state).unwrap();
            Self { home, data, state }
        }

        fn vault(&self) -> PathBuf {
            self.home.join(".local/share/crystal/diary")
        }

        fn dir(&self) -> VaultDir {
            unlock::open_vault_dir(&self.data, &name()).unwrap()
        }

        fn unlock(&self, pw: &str) -> Result<Unlocked, unlock::UnlockError> {
            unlock::unlock(&self.dir(), &name(), pass(pw), &self.state)
        }

        /// Whether the identity in `identity_age` (sealed to `pw`), such
        /// as an old copy from a backup, opens today's vault.age.
        fn opens(&self, identity_age: &[u8], pw: &str) -> bool {
            let identity = VaultIdentity::open_with_passphrase(identity_age, &pass(pw)).unwrap();
            let vault_age = fs::read(self.vault().join(VAULT_FILE)).unwrap();
            format::open(&vault_age, &identity, FORMAT).is_ok()
        }

        fn identity_age(&self) -> Vec<u8> {
            fs::read(self.vault().join(IDENTITY_FILE)).unwrap()
        }

        fn counter(&self, unlocked: &Unlocked) -> Option<u64> {
            generation::load(self.state.fd(), Counter::Vault(unlocked.contents.vault_id)).unwrap()
        }
    }

    /// A passphrase change on `world`'s vault, unlocked with `pw`.
    fn rekey<'a>(world: &'a World, dir: &'a mut VaultDir, pw: &str) -> Rekey<'a> {
        let Unlocked {
            contents,
            recipient,
            stamp,
            ..
        } = world.unlock(pw).unwrap();
        Rekey::new(dir, &world.state, contents, recipient, stamp).unwrap()
    }

    // The whole change: the new passphrase opens the vault, the old one
    // no longer does, and an old copy of identity.age (a backup) opens
    // nothing written afterwards. The notes and the vault id stay.
    #[test]
    fn changes_the_passphrase_and_the_key() {
        let world = World::new("full");
        let old_copy = world.identity_age();
        let before = world.unlock(OLD).unwrap();
        let mut dir = world.dir();
        rekey(&world, &mut dir, OLD).run(&pass(NEW)).unwrap();

        let after = world.unlock(NEW).unwrap();
        assert!(after.contents.vault_id == before.contents.vault_id);
        assert_eq!(after.contents.notes.len(), before.contents.notes.len());
        assert_eq!(after.contents.generation, before.contents.generation + 2);
        assert_eq!(world.counter(&after), Some(before.contents.generation + 2));
        assert!(matches!(
            world.unlock(OLD).err(),
            Some(unlock::UnlockError::Identity(CryptoError::WrongPassphrase))
        ));
        assert!(
            !world.opens(&old_copy, OLD),
            "the old key still opens vault.age"
        );
    }

    // A crash after step 1: identity.age is the old one and opens the
    // vault, which the new identity would open too.
    #[test]
    fn after_step_one_the_old_passphrase_opens_it() {
        let world = World::new("step1");
        let mut dir = world.dir();
        let mut change = rekey(&world, &mut dir, OLD);
        change.seal_to_both().unwrap();
        drop(change);
        let unlocked = world.unlock(OLD).unwrap();
        assert_eq!(unlocked.contents.generation, 2);
        assert_eq!(world.counter(&unlocked), Some(2));
    }

    // A crash after step 2: the new passphrase opens the vault; the old
    // identity still opens vault.age until the next write to it, and a
    // second `crystal passwd` finishes the job.
    #[test]
    fn after_step_two_the_new_passphrase_opens_it() {
        let world = World::new("step2");
        let old_copy = world.identity_age();
        let mut dir = world.dir();
        let mut change = rekey(&world, &mut dir, OLD);
        change.seal_to_both().unwrap();
        change.seal_identity(&pass(NEW)).unwrap();
        drop(change);
        assert!(world.unlock(NEW).is_ok());
        assert!(world.unlock(OLD).is_err());
        assert!(world.opens(&old_copy, OLD));

        let mut dir = world.dir();
        rekey(&world, &mut dir, NEW).run(&pass(OLD)).unwrap();
        assert!(!world.opens(&old_copy, OLD));
    }

    // A failed write stops the change, and the error says which
    // passphrase opens the vault; that passphrase does.
    #[test]
    fn a_failed_write_says_which_passphrase_opens_the_vault() {
        for (step, file, reached, works) in [
            (Step::Rename, VAULT_FILE, Reached::Old, OLD),
            (Step::DirFsync, VAULT_FILE, Reached::Old, OLD),
            (Step::Rename, IDENTITY_FILE, Reached::Old, OLD),
            (Step::DirFsync, IDENTITY_FILE, Reached::NewUnconfirmed, NEW),
        ] {
            let world = World::new(&format!("fail_{step:?}_{file}"));
            let mut dir = world.dir();
            let change = rekey(&world, &mut dir, OLD);
            let err = {
                let _armed = FailAt::arm_for(step, file);
                change.run(&pass(NEW)).unwrap_err()
            };
            assert_eq!(err.reached, reached, "{step:?} {file}");
            assert!(world.unlock(works).is_ok(), "{step:?} {file}");
        }
    }

    // Step 3 failing leaves the new passphrase in force and says the old
    // key still opens vault.age.
    #[test]
    fn a_failed_last_step_says_the_old_key_still_opens_vault_age() {
        let world = World::new("fail_step3");
        let mut dir = world.dir();
        let mut change = rekey(&world, &mut dir, OLD);
        change.seal_to_both().unwrap();
        change.seal_identity(&pass(NEW)).unwrap();
        let err = {
            let _armed = FailAt::arm_for(Step::Rename, VAULT_FILE);
            change.seal_to_new().unwrap_err()
        };
        assert_eq!(err.reached, Reached::NewWithOldKey);
        assert!(err.to_string().ends_with("run crystal passwd again"));
        drop(change);
        assert!(world.unlock(NEW).is_ok());
    }

    // A vault.age replaced on disk since it was read (a sync) is never
    // overwritten: nothing is written, and the old passphrase still works.
    #[test]
    fn refuses_a_vault_that_changed_on_disk() {
        let world = World::new("changed");
        let mut dir = world.dir();
        let change = rekey(&world, &mut dir, OLD);
        let identity_before = world.identity_age();
        let vault_age = world.vault().join(VAULT_FILE);
        let bytes = fs::read(&vault_age).unwrap();
        fs::remove_file(&vault_age).unwrap();
        fs::write(&vault_age, &bytes).unwrap();

        let err = change.run(&pass(NEW)).unwrap_err();
        assert_eq!(
            err,
            PasswdError {
                cause: Cause::Changed,
                reached: Reached::Old
            }
        );
        assert_eq!(world.identity_age(), identity_before);
        assert!(world.unlock(OLD).is_ok());
    }

    #[test]
    fn messages_are_fixed_text() {
        for reached in [
            Reached::Old,
            Reached::NewUnconfirmed,
            Reached::NewWithOldKey,
            Reached::Done,
        ] {
            let shown = PasswdError {
                cause: Cause::Changed,
                reached,
            }
            .to_string();
            assert!(!shown.contains("diary") && !shown.contains(NEW), "{shown}");
        }
    }
}
