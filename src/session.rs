//! The open vault: its notes, and the one function that saves them
//! (decision 0045, "Saving"; docs/design/storage.md,
//! "Save sequence").
//!
//! A save writes the whole vault: generation + 1, seal to the recipient,
//! `vault::atomic_write`, then the rollback counter, then the indexes.
//! Before anything is written:
//!
//! - `vault.age` must still be the file unlock decrypted (its stamp),
//!   checked after sealing, the last step before anything is written. If
//!   Syncthing or a second Crystal replaced it, the save is refused and
//!   nothing is written; Crystal never merges two versions. The check
//!   cannot be atomic with the rename: a replacement landing between the
//!   two is overwritten.
//! - A directory whose `VERSION` still says format 1 gets `VERSION` 2
//!   first (decision 0034), so a crash between the
//!   two writes leaves a readable vault.
//!
//! A save that fails before the rename changes nothing on disk, and the
//! change is undone in memory too, so memory always matches the vault
//! (the editor keeps the text). Two failures come after the commit: the
//! directory sync and the rollback counter. Then the vault is saved, and
//! the error says so (`SaveError::is_saved`).

use std::fmt;

use rustix::io::Errno;
use zeroize::Zeroizing;

use crate::crypto::VaultRecipient;
use crate::model::{Note, NoteId, NoteText};
use crate::notes::Notes;
use crate::vault::StateDir;
use crate::vault::atomic::{FileName, Payload, Step, WriteError, atomic_write};
use crate::vault::codec::FORMAT;
use crate::vault::format::{self, VAULT_FILE, VaultError};
use crate::vault::generation::{self, Counter, GenError};
use crate::vault::public_file::{FileStamp, stamp};
use crate::vault::vault_dir::VaultDir;

/// One change, saved at once.
pub enum Change {
    /// The editor's text for a note.
    Body { note: usize, text: NoteText },
    /// A new note (`new NAME`), its name already checked. Every `[[link]]`
    /// the new name would capture or make ambiguous is rewritten in the
    /// same save, so it keeps leading where it did (`Notes::relinked`).
    New {
        folder: NoteText,
        title: NoteText,
        body: NoteText,
    },
    /// `mv`: a new name, its name already checked. Every `[[link]]` that
    /// led to the note, or that the new name would make ambiguous, is
    /// rewritten in the same save (`Notes::relinked`).
    Rename {
        note: usize,
        folder: NoteText,
        title: NoteText,
    },
    /// `rm`: the note is gone for good (no trash, decision 0032).
    Remove { note: usize },
}

/// Notes whose links a save rewrote: their old text and time.
type Bodies = Vec<(usize, NoteText, u64)>;

/// What undoes an applied change if the save fails.
enum Undo {
    Body {
        note: usize,
        text: NoteText,
        modified: u64,
    },
    New {
        bodies: Bodies,
    },
    Rename {
        note: usize,
        folder: NoteText,
        title: NoteText,
        modified: u64,
        bodies: Bodies,
    },
    Remove {
        note: usize,
        removed: Note,
    },
}

/// Why a save failed. Fixed messages and numbers only, never note text.
#[derive(Debug)]
pub enum SaveError {
    /// `vault.age` is not the file this session read.
    Changed,
    Stamp(Errno),
    NoSuchNote,
    Random(Errno),
    GenerationExhausted,
    Version(WriteError),
    Seal(VaultError),
    Write(WriteError),
    /// Saved, but the directory sync failed: may not survive a power loss.
    NotDurable(WriteError),
    /// Saved, but the rollback counter was not updated.
    Counter(GenError),
}

impl SaveError {
    /// Whether the change reached `vault.age` despite the error.
    pub fn is_saved(&self) -> bool {
        matches!(self, Self::NotDurable(_) | Self::Counter(_))
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed => f.write_str(
                "not saved: vault.age changed on disk since it was opened (synced from another \
                 device?). Nothing was written. Your edits exist only in the editor and are lost \
                 when Crystal closes; open the vault again to see the other version",
            ),
            Self::Stamp(errno) => write!(f, "not saved: cannot look at vault.age ({errno})"),
            Self::NoSuchNote => f.write_str("not saved: that note no longer exists"),
            Self::Random(errno) => write!(f, "not saved: no random note id ({errno})"),
            Self::GenerationExhausted => f.write_str("not saved: the generation counter is full"),
            Self::Version(e) => write!(f, "not saved: VERSION: {e}"),
            Self::Seal(e) => write!(f, "not saved: {e}"),
            Self::Write(e) => write!(f, "not saved: vault.age: {e}"),
            // The counter waits for a durable rename: if a power loss undid
            // it, a counter ahead of the file would look like a rollback.
            Self::NotDurable(e) => write!(
                f,
                "saved, but {e}: it may not survive a power loss, so the rollback counter was \
                 not updated (the next unlock catches it up)"
            ),
            Self::Counter(e) => write!(f, "saved, but the rollback counter was not updated: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// An unlocked vault for the length of a session. No Debug: it holds the
/// notes and the recipient.
pub struct Session {
    vault_dir: VaultDir,
    state: StateDir,
    recipient: VaultRecipient,
    /// The `vault.age` on disk that matches the notes in memory.
    stamp: FileStamp,
    notes: Notes,
}

impl Session {
    /// From what `unlock::unlock` returned: `notes` are its contents with
    /// their indexes.
    pub fn new(
        vault_dir: VaultDir,
        state: StateDir,
        recipient: VaultRecipient,
        stamp: FileStamp,
        notes: Notes,
    ) -> Self {
        Self {
            vault_dir,
            state,
            recipient,
            stamp,
            notes,
        }
    }

    pub fn notes(&self) -> &Notes {
        &self.notes
    }

    /// Lock (decision 0048): ends the session
    /// and keeps only the vault and state directory handles, which are
    /// not secret, to unlock again. The notes, their indexes and the
    /// recipient are dropped here, and each wipes itself.
    pub fn into_dirs(self) -> (VaultDir, StateDir) {
        let Self {
            vault_dir, state, ..
        } = self;
        (vault_dir, state)
    }

    /// Applies `change` and saves the vault. On `Err`, unless
    /// `is_saved()`, nothing changed on disk or in memory.
    pub fn save(&mut self, change: Change) -> Result<(), SaveError> {
        let generation = self
            .notes
            .contents
            .generation
            .checked_add(1)
            .ok_or(SaveError::GenerationExhausted)?;
        let undo = self.apply(change, now())?;
        let old_generation = std::mem::replace(&mut self.notes.contents.generation, generation);
        if let Err(e) = self.write() {
            if !e.is_saved() {
                self.notes.contents.generation = old_generation;
                self.revert(undo);
                return Err(e);
            }
            self.after_commit();
            return Err(e);
        }
        // Saved: the old text in `undo` is dropped, which wipes it.
        drop(undo);
        self.after_commit();
        let counter = Counter::Vault(self.notes.contents.vault_id);
        generation::record_save(self.state.fd(), counter, generation).map_err(SaveError::Counter)
    }

    /// Seals, checks the stamp, then writes `VERSION` if needed and
    /// `vault.age`. Errors from `DirFsync` on `vault.age` come after the
    /// commit (`NotDurable`).
    fn write(&mut self) -> Result<(), SaveError> {
        let sealed =
            format::seal(&self.notes.contents, &self.recipient).map_err(SaveError::Seal)?;
        // As late as it can be without writing anything: the window for a
        // synced file to land unseen is the two writes below, not the seal.
        let current = stamp(self.vault_dir.fd(), VAULT_FILE).map_err(SaveError::Stamp)?;
        if current != Some(self.stamp) {
            return Err(SaveError::Changed);
        }
        if self.vault_dir.version() < FORMAT {
            format::write_version(self.vault_dir.fd()).map_err(SaveError::Version)?;
            self.vault_dir.set_version(FORMAT);
        }
        let fd = self.vault_dir.fd();
        // A fixed, valid name: FileName::new cannot refuse it.
        let name = FileName::new(VAULT_FILE).ok_or(SaveError::Write(WriteError {
            step: Step::Open,
            errno: Errno::INVAL,
        }))?;
        atomic_write(fd, &name, Payload::Ciphertext(&sealed)).map_err(|e| match e.step {
            Step::DirFsync => SaveError::NotDurable(e),
            _ => SaveError::Write(e),
        })
    }

    /// After `vault.age` was replaced: remember the new file, rebuild the
    /// indexes. If the new file cannot be looked at, the stamp is left
    /// stale, so the next save is refused rather than risk overwriting.
    fn after_commit(&mut self) {
        if let Ok(Some(new)) = stamp(self.vault_dir.fd(), VAULT_FILE) {
            self.stamp = new;
        }
        self.notes.reindex();
    }

    fn apply(&mut self, change: Change, now: u64) -> Result<Undo, SaveError> {
        // Worked out on the notes as they are, before anything changes.
        let relinked = match &change {
            Change::Rename {
                note,
                folder,
                title,
            } if *note < self.notes.len() => {
                self.notes.relinked(*note, folder.as_str(), title.as_str())
            }
            // The new note's index once pushed: no link leads there yet.
            Change::New { folder, title, .. } => {
                self.notes
                    .relinked(self.notes.len(), folder.as_str(), title.as_str())
            }
            _ => Vec::new(),
        };
        let notes = &mut self.notes.contents.notes;
        match change {
            Change::Body { note, mut text } => {
                let target = notes.get_mut(note).ok_or(SaveError::NoSuchNote)?;
                std::mem::swap(&mut target.body, &mut text);
                let modified = std::mem::replace(&mut target.modified, now);
                Ok(Undo::Body {
                    note,
                    text,
                    modified,
                })
            }
            Change::New {
                folder,
                title,
                body,
            } => {
                // Before any rewrite, so a failure here changes nothing.
                let id = NoteId::random().map_err(SaveError::Random)?;
                let bodies = rewrite(notes, relinked, now);
                notes.push(Note {
                    id,
                    folder,
                    title,
                    created: now,
                    modified: now,
                    body,
                });
                Ok(Undo::New { bodies })
            }
            Change::Rename {
                note,
                mut folder,
                mut title,
            } => {
                let target = notes.get_mut(note).ok_or(SaveError::NoSuchNote)?;
                std::mem::swap(&mut target.folder, &mut folder);
                std::mem::swap(&mut target.title, &mut title);
                let modified = std::mem::replace(&mut target.modified, now);
                let bodies = rewrite(notes, relinked, now);
                Ok(Undo::Rename {
                    note,
                    folder,
                    title,
                    modified,
                    bodies,
                })
            }
            Change::Remove { note } => {
                if note >= notes.len() {
                    return Err(SaveError::NoSuchNote);
                }
                Ok(Undo::Remove {
                    note,
                    removed: notes.remove(note),
                })
            }
        }
    }

    fn revert(&mut self, undo: Undo) {
        let notes = &mut self.notes.contents.notes;
        match undo {
            Undo::Body {
                note,
                mut text,
                modified,
            } => {
                if let Some(target) = notes.get_mut(note) {
                    std::mem::swap(&mut target.body, &mut text);
                    target.modified = modified;
                }
            }
            Undo::New { bodies } => {
                notes.pop();
                restore(notes, bodies);
            }
            Undo::Rename {
                note,
                mut folder,
                mut title,
                modified,
                bodies,
            } => {
                restore(notes, bodies);
                if let Some(target) = notes.get_mut(note) {
                    std::mem::swap(&mut target.folder, &mut folder);
                    std::mem::swap(&mut target.title, &mut title);
                    target.modified = modified;
                }
            }
            Undo::Remove { note, removed } => notes.insert(note, removed),
        }
    }
}

/// Puts `relinked`'s new texts into their notes; returns the old texts
/// and times, for `restore`. `relinked` only names notes that exist.
fn rewrite(notes: &mut [Note], relinked: Vec<(usize, Zeroizing<String>)>, now: u64) -> Bodies {
    let mut bodies = Vec::with_capacity(relinked.len());
    for (index, text) in relinked {
        if let Some(other) = notes.get_mut(index) {
            let mut text = NoteText::from_zeroizing(text);
            std::mem::swap(&mut other.body, &mut text);
            let modified = std::mem::replace(&mut other.modified, now);
            bodies.push((index, text, modified));
        }
    }
    bodies
}

/// Undoes `rewrite`.
fn restore(notes: &mut [Note], bodies: Bodies) {
    for (index, mut text, modified) in bodies {
        if let Some(other) = notes.get_mut(index) {
            std::mem::swap(&mut other.body, &mut text);
            other.modified = modified;
        }
    }
}

/// Seconds since the Unix epoch (`clock_gettime`); 0 if the clock is set
/// before 1970.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Passphrase;
    use crate::init;
    use crate::unlock;
    use crate::vault::atomic::FailAt;
    use crate::vault::{DataDir, VaultName};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    fn text(s: &str) -> NoteText {
        NoteText::from_zeroizing(Zeroizing::new(s.to_owned()))
    }

    fn name() -> VaultName {
        VaultName::new("diary".into()).unwrap()
    }

    fn pass() -> Passphrase {
        Passphrase::new(Zeroizing::new(b"pw".to_vec())).unwrap()
    }

    /// A private HOME with a vault made by `crystal init`'s own code.
    fn home(test: &str) -> PathBuf {
        let home = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-session")
            .join(test);
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let data = DataDir::create(Some(home.as_os_str())).unwrap();
        let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
        let plan = init::plan(&data, &name()).unwrap();
        init::create(plan, &name(), &pass(), &data, &state).unwrap();
        home
    }

    fn open(home: &Path) -> Session {
        let data = DataDir::open(Some(home.as_os_str())).unwrap();
        let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
        let dir = unlock::open_vault_dir(&data, &name()).unwrap();
        let u = unlock::unlock(&dir, &name(), pass(), &state).unwrap();
        let notes = Notes::new(u.contents);
        Session::new(dir, state, u.recipient, u.stamp, notes)
    }

    fn vault(home: &Path) -> PathBuf {
        home.join(".local/share/crystal/diary")
    }

    fn new_note(title: &str, body: &str) -> Change {
        Change::New {
            folder: text(""),
            title: text(title),
            body: text(body),
        }
    }

    #[test]
    fn saves_and_reopens() {
        let home = home("saves");
        let mut s = open(&home);
        let generation = s.notes().contents.generation;
        s.save(new_note("plan", "first")).unwrap();
        s.save(Change::Body {
            note: 0,
            text: text("second"),
        })
        .unwrap();
        assert_eq!(s.notes().name(0), "plan", "indexes rebuilt");

        let s = open(&home);
        assert_eq!(s.notes().len(), 1);
        assert_eq!(s.notes().note(0).body.as_str(), "second");
        assert_eq!(s.notes().contents.generation, generation + 2);
        let counter = Counter::Vault(s.notes().contents.vault_id);
        assert_eq!(
            generation::load(s.state.fd(), counter).unwrap(),
            Some(generation + 2),
            "the rollback counter follows each save"
        );
    }

    /// Syncthing (or a second Crystal) replaced vault.age: the save is
    /// refused, nothing is written (not even a due VERSION 2), and memory
    /// is as before.
    #[test]
    fn refuses_to_replace_a_vault_that_changed_on_disk() {
        let home = home("changed");
        let mut s = open(&home);
        let version = vault(&home).join("VERSION");
        fs::write(&version, "1\n").unwrap();
        s.vault_dir.set_version(1);
        let file = vault(&home).join("vault.age");
        let bytes = fs::read(&file).unwrap();
        // The same bytes, written anew: a new version of the file all the
        // same (Syncthing replaces files by renaming a copy).
        fs::write(vault(&home).join("synced"), &bytes).unwrap();
        fs::rename(vault(&home).join("synced"), &file).unwrap();

        let e = s.save(new_note("plan", "text")).unwrap_err();
        assert!(matches!(e, SaveError::Changed), "{e}");
        assert!(!e.is_saved());
        assert_eq!(s.notes().len(), 0);
        assert_eq!(fs::read(&file).unwrap(), bytes, "vault.age untouched");
        assert_eq!(
            fs::read_to_string(&version).unwrap(),
            "1\n",
            "VERSION untouched"
        );
    }

    /// A failed write changes nothing on disk or in memory; the save can be
    /// tried again.
    #[test]
    fn a_failed_write_undoes_the_change_in_memory() {
        let home = home("failed");
        let mut s = open(&home);
        s.save(new_note("plan", "kept")).unwrap();
        let generation = s.notes().contents.generation;
        let bytes = fs::read(vault(&home).join("vault.age")).unwrap();
        for step in [Step::Write, Step::Fsync, Step::Rename] {
            let _fail = FailAt::arm_for(step, "vault.age");
            let change = Change::Body {
                note: 0,
                text: text("lost"),
            };
            assert!(
                matches!(s.save(change), Err(SaveError::Write(_))),
                "{step:?}"
            );
            assert_eq!(s.notes().note(0).body.as_str(), "kept");
            assert_eq!(s.notes().contents.generation, generation);
            assert_eq!(fs::read(vault(&home).join("vault.age")).unwrap(), bytes);
        }
        let _fail = FailAt::arm_for(Step::Write, "vault.age");
        assert!(s.save(new_note("other", "")).is_err());
        assert_eq!(s.notes().len(), 1, "the new note is gone again");
        drop(_fail);
        s.save(Change::Body {
            note: 0,
            text: text("saved"),
        })
        .unwrap();
        assert_eq!(open(&home).notes().note(0).body.as_str(), "saved");
    }

    /// The directory sync fails after the rename: the vault is saved, and
    /// the change stays in memory. The rollback counter stays behind, so a
    /// power loss that undid the rename is not taken for a rollback.
    #[test]
    fn a_failed_directory_sync_is_saved_with_a_warning() {
        let home = home("dirfsync");
        let mut s = open(&home);
        let counter = Counter::Vault(s.notes().contents.vault_id);
        let before = generation::load(s.state.fd(), counter).unwrap();
        let e = {
            let _fail = FailAt::arm_for(Step::DirFsync, "vault.age");
            s.save(new_note("plan", "on disk")).unwrap_err()
        };
        assert!(e.is_saved(), "{e}");
        assert!(e.to_string().contains("counter was not updated"), "{e}");
        assert_eq!(generation::load(s.state.fd(), counter).unwrap(), before);
        assert_eq!(s.notes().len(), 1);
        // The stamp follows the new file, so the next save is not refused.
        s.save(Change::Body {
            note: 0,
            text: text("again"),
        })
        .unwrap();
        assert_eq!(open(&home).notes().note(0).body.as_str(), "again");
    }

    /// A directory whose VERSION says 1 gets VERSION 2 before vault.age;
    /// if vault.age then fails, VERSION 2 with the old vault still opens.
    #[test]
    fn writes_version_before_the_first_format_2_vault() {
        let home = home("version");
        let version = vault(&home).join("VERSION");
        let mut s = open(&home);
        fs::write(&version, "1\n").unwrap();
        s.vault_dir.set_version(1);
        {
            let _fail = FailAt::arm_for(Step::Write, "vault.age");
            assert!(s.save(new_note("plan", "")).is_err());
        }
        assert_eq!(
            fs::read_to_string(&version).unwrap(),
            "2\n",
            "VERSION first"
        );
        assert_eq!(open(&home).notes().len(), 0, "the old vault opens");
        s.save(new_note("plan", "")).unwrap();
        assert_eq!(open(&home).notes().len(), 1);
    }

    /// `mv` saves the new name and the rewritten links together; a failed
    /// write undoes both in memory.
    #[test]
    fn renames_with_its_links_and_undoes_both() {
        let home = home("rename");
        let mut s = open(&home);
        s.save(new_note("plan", "")).unwrap();
        s.save(new_note("index", "See [[plan|the plan]].")).unwrap();
        let rename = || Change::Rename {
            note: 0,
            folder: text("work"),
            title: text("goals"),
        };
        {
            let _fail = FailAt::arm_for(Step::Write, "vault.age");
            assert!(s.save(rename()).is_err());
        }
        assert_eq!(s.notes().name(0), "plan");
        assert_eq!(s.notes().note(1).body.as_str(), "See [[plan|the plan]].");
        s.save(rename()).unwrap();

        let s = open(&home);
        assert_eq!(s.notes().name(0), "work/goals");
        assert_eq!(s.notes().note(1).body.as_str(), "See [[goals|the plan]].");
    }

    /// A new note whose title another note has: bare links to that note
    /// get its full name in the same save, so they stay unambiguous; a
    /// failed write undoes both.
    #[test]
    fn a_new_note_keeps_links_unambiguous_and_undoes_both() {
        let home = home("new_relink");
        let mut s = open(&home);
        s.save(Change::New {
            folder: text("a"),
            title: text("plan"),
            body: text(""),
        })
        .unwrap();
        s.save(new_note("index", "See [[plan]].")).unwrap();
        let second = || Change::New {
            folder: text("b"),
            title: text("plan"),
            body: text(""),
        };
        {
            let _fail = FailAt::arm_for(Step::Write, "vault.age");
            assert!(s.save(second()).is_err());
        }
        assert_eq!(s.notes().len(), 2);
        assert_eq!(s.notes().note(1).body.as_str(), "See [[plan]].");
        s.save(second()).unwrap();

        let s = open(&home);
        assert_eq!(s.notes().len(), 3);
        assert_eq!(s.notes().note(1).body.as_str(), "See [[a/plan]].");
    }

    /// `rm` deletes the note in one save; a failed write puts it back.
    #[test]
    fn removes_and_undoes_a_failed_removal() {
        let home = home("remove");
        let mut s = open(&home);
        s.save(new_note("a", "first")).unwrap();
        s.save(new_note("b", "second")).unwrap();
        {
            let _fail = FailAt::arm_for(Step::Write, "vault.age");
            assert!(s.save(Change::Remove { note: 0 }).is_err());
        }
        assert_eq!(s.notes().len(), 2);
        assert_eq!(
            s.notes().note(0).body.as_str(),
            "first",
            "back in its place"
        );
        s.save(Change::Remove { note: 0 }).unwrap();
        assert!(matches!(
            s.save(Change::Remove { note: 5 }),
            Err(SaveError::NoSuchNote)
        ));

        let s = open(&home);
        assert_eq!(s.notes().len(), 1);
        assert_eq!(s.notes().name(0), "b");
    }

    /// What memory holds, to compare: the generation and every note's
    /// name and text, in order.
    fn snapshot(s: &Session) -> (u64, Vec<(String, String)>) {
        let notes = s.notes();
        let all = (0..notes.len())
            .map(|i| {
                (
                    notes.name(i).to_owned(),
                    notes.note(i).body.as_str().to_owned(),
                )
            })
            .collect();
        (notes.contents.generation, all)
    }

    const FOLDERS: [&str; 2] = ["", "work"];
    const TITLES: [&str; 4] = ["plan", "Plan", "index", "goals"];
    const BODIES: [&str; 4] = ["", "See [[plan]].", "[[goals|g]], [[work/plan]]", "x"];
    const STEPS: [Step; 7] = [
        Step::Sweep,
        Step::Random,
        Step::Open,
        Step::Write,
        Step::Fsync,
        Step::Rename,
        Step::DirFsync,
    ];

    /// One change from four small numbers, or None when it would make a
    /// name the shell refuses (taken, ignoring case) or there is no note.
    fn change(s: &Session, (kind, a, b, c): (u8, usize, usize, usize)) -> Option<Change> {
        let notes = s.notes();
        let (folder, title) = (FOLDERS[a % 2], TITLES[b % 4]);
        if kind == 0 {
            return (!notes.exists(folder, title)).then(|| Change::New {
                folder: text(folder),
                title: text(title),
                body: text(BODIES[c % 4]),
            });
        }
        let note = a.checked_rem(notes.len())?;
        Some(match kind {
            1 if notes.exists(folder, title) => return None,
            1 => Change::Rename {
                note,
                folder: text(folder),
                title: text(title),
            },
            2 => Change::Body {
                note,
                text: text(BODIES[c % 4]),
            },
            _ => Change::Remove { note },
        })
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(32))]

        /// Any changes, each save failing at any step of any write or
        /// not at all: a save that reports nothing saved left memory and
        /// vault.age exactly as they were; any other leaves memory equal
        /// to what a fresh unlock reads.
        #[test]
        fn memory_always_matches_the_vault(
            ops in proptest::collection::vec(
                ((0..4u8, 0..8usize, 0..8usize, 0..8usize), 0..14usize),
                1..7,
            ),
        ) {
            let home = home("prop");
            let file = vault(&home).join("vault.age");
            let mut s = open(&home);
            for (op, fail) in ops {
                let Some(change) = change(&s, op) else { continue };
                let (before, bytes) = (snapshot(&s), fs::read(&file).unwrap());
                let result = {
                    // Armed for every file: the counter's write fails too.
                    let _fail = STEPS.get(fail).map(|&step| FailAt::arm(step));
                    s.save(change)
                };
                match result {
                    Err(e) if !e.is_saved() => {
                        proptest::prop_assert_eq!(&snapshot(&s), &before, "{}", e);
                        proptest::prop_assert_eq!(fs::read(&file).unwrap(), bytes);
                    }
                    _ => proptest::prop_assert_eq!(snapshot(&open(&home)), snapshot(&s)),
                }
            }
        }
    }

    // ---- Phase 5 acceptance: no note text in memory after lock ----

    /// The canary, XOR-masked at compile time: the plain text exists only
    /// where the code under test puts it, never in the search itself.
    const MASK: u8 = 0x5a;
    const CANARY_MASKED: [u8; 23] = {
        let plain = *b"crystal-canary-7d41e9b3";
        let mut masked = [0; 23];
        let mut i = 0;
        while i < plain.len() {
            masked[i] = plain[i] ^ MASK;
            i += 1;
        }
        masked
    };

    fn canary() -> Zeroizing<String> {
        let mut text = Zeroizing::new(String::with_capacity(CANARY_MASKED.len()));
        for b in CANARY_MASKED {
            text.push(char::from(b ^ MASK));
        }
        text
    }

    /// Where in this process's writable memory (heap, anonymous mappings,
    /// stacks, the binary's data) `masked` (XORed with `MASK`) is, read through
    /// /proc/self/mem: a page that went away in the meantime is an error
    /// to skip, never a crash. Read-only mappings (the code, constants)
    /// are skipped: nothing can be written there.
    fn in_memory(masked: &[u8]) -> Option<String> {
        use std::os::unix::fs::FileExt;
        let maps = fs::read_to_string("/proc/self/maps").unwrap();
        let mem = fs::File::open("/proc/self/mem").expect(
            "cannot open /proc/self/mem: this process is non-dumpable (run through \
             no_note_text_in_memory_after_lock, which starts a fresh one)",
        );
        let needle = masked.len();
        let mut buf = vec![0u8; 1 << 20];
        let mut found = None;
        'regions: for line in maps.lines() {
            let mut fields = line.split_whitespace();
            let (Some(range), Some(perms)) = (fields.next(), fields.next()) else {
                continue;
            };
            if !perms.starts_with("rw") || line.contains("[vvar") || line.contains("[vsyscall]") {
                continue;
            }
            let (start, end) = range.split_once('-').unwrap();
            let (start, end) = (
                u64::from_str_radix(start, 16).unwrap(),
                u64::from_str_radix(end, 16).unwrap(),
            );
            let mut at = start;
            while at < end {
                let n = usize::try_from(end - at)
                    .unwrap_or(usize::MAX)
                    .min(buf.len());
                if mem.read_exact_at(&mut buf[..n], at).is_err() {
                    break;
                }
                let hit = buf[..n]
                    .windows(needle)
                    .position(|w| w.iter().zip(masked).all(|(&b, &m)| b ^ MASK == m));
                if let Some(offset) = hit {
                    found = Some(format!("{line} (+{:#x})", at - start + offset as u64));
                    break 'regions;
                }
                if n <= needle {
                    break;
                }
                // Overlap the next read, so a canary across two reads is found.
                at += (n - (needle - 1)) as u64;
            }
        }
        zeroize::Zeroize::zeroize(&mut buf);
        found
    }

    /// Crystal's parts, used as a session uses them, in this process: a
    /// vault with a note holding the canary (in its title, its text, a
    /// heading, a tag and a link), saved; the shell's completion names,
    /// `search CANARY` typed into the shell (line, history, output); the
    /// pager's rendering; the editor, typed into (its link suggestions
    /// open on the canary's note, drawn and taken), and Crystal's clipboard
    /// filled from it. Then what `lock` does (decision 0048), step by step
    /// as `app::run` and `crystal::run` (src/lib.rs) do it: the editor, pager and grid
    /// dropped, the clipboard wiped, the shell dropped, the session ended
    /// with only its directory handles kept. Afterwards no writable byte
    /// of the process holds the canary, nor the vault key's text form.
    /// Run by `no_note_text_in_memory_after_lock` in a fresh, dumpable
    /// process: Crystal itself is non-dumpable, so even it cannot open its
    /// own /proc/self/mem. What the scan cannot see is in
    /// docs/development/testing.md ("Canary scan").
    #[test]
    #[ignore = "run by no_note_text_in_memory_after_lock, in a fresh process"]
    fn canary_child_lock_leaves_no_note_text() {
        use crate::commands;
        use crate::editor::Editor;
        use crate::editor::view::View;
        use crate::pager::Pager;
        use crate::shell::Shell;
        use crate::shell::keys::Key;
        use crate::ui::clipboard::Clipboard;
        use crate::ui::grid::Grid;

        assert!(fs::File::open("/proc/self/mem").is_ok(), "not dumpable");
        let home = home("canary");
        let mut session = open(&home);
        let dirs = {
            let canary = canary();
            let mut body = Zeroizing::new(String::new());
            for part in [
                "# Plan ",
                &canary,
                "\n\nText ",
                &canary,
                " #t",
                &canary,
                " [[",
                &canary,
                "]]\n",
            ] {
                body.push_str(part);
            }
            // The title too, so the name index and the shell's completion
            // names hold the canary.
            session.save(new_note(&canary, &body)).unwrap();

            // The shell as in crystal::run (src/lib.rs): the names for completion, then
            // `search CANARY` typed key by key, so the line editor, the
            // history and the command's words hold it; Up recalls it into
            // the line again.
            let mut shell = Shell::new(&name());
            shell.set_names(crate::app::names(session.notes()));
            for c in "search ".chars().chain(canary.chars()) {
                shell.key(Key::Char(c));
            }
            let crate::shell::Action::Command(words) = shell.key(Key::Enter) else {
                panic!("search was not a command");
            };
            commands::run(session.notes(), &words, &mut shell);
            shell.key(Key::Up);
            let mut grid = Grid::new(80, 24);
            shell.draw(&mut grid);

            let mut pager = Pager::open(0);
            pager.draw(&mut grid, session.notes(), None);

            // The editor as app::run drives it, with link suggestions:
            // `[[` and the canary open the list on the canary's note; it is
            // drawn, and Enter links the note, so its name is copied into
            // the text.
            let mut clipboard = Clipboard::default();
            let mut editor = Editor::new(&body).unwrap();
            let mut view = View::default();
            // On a new first line, so `# Plan …` stays a heading.
            editor.key_in(Key::Enter, &mut clipboard, session.notes());
            editor.key_in(Key::Up, &mut clipboard, session.notes());
            for c in "[[".chars().chain(canary.chars()) {
                editor.key_in(Key::Char(c), &mut clipboard, session.notes());
            }
            assert!(editor.suggestions().is_some(), "the list is open");
            view.draw(&mut grid, &mut editor, "plan");
            view.draw_suggestions(&mut grid, &editor, session.notes());
            editor.key_in(Key::Enter, &mut clipboard, session.notes());
            // `[[#` scans the text's own headings (the canary is in its
            // heading) into the list, which Enter takes.
            for c in " [[#".chars() {
                editor.key_in(Key::Char(c), &mut clipboard, session.notes());
            }
            assert!(editor.suggestions().is_some(), "the heading list is open");
            view.draw(&mut grid, &mut editor, "plan");
            view.draw_suggestions(&mut grid, &editor, session.notes());
            editor.key_in(Key::Enter, &mut clipboard, session.notes());
            editor.key_in(Key::Ctrl('a'), &mut clipboard, session.notes());
            editor.key_in(Key::Ctrl('c'), &mut clipboard, session.notes());
            view.draw(&mut grid, &mut editor, "plan");
            assert!(
                in_memory(&CANARY_MASKED).is_some(),
                "the scan finds a live canary"
            );

            // What lock does.
            drop((editor, view, pager, grid));
            clipboard.wipe();
            drop(clipboard);
            drop(shell);
            drop(words);
            session.into_dirs()
        };
        if let Some(at) = in_memory(&CANARY_MASKED) {
            panic!("note text left in memory after lock: {at}");
        }
        // The vault key's text form (`AGE-SECRET-KEY-1…`, docs/design/leakage.md):
        // unlock parsed it from identity.age and wiped it. The test opens
        // identity.age itself and keeps only a masked copy to look for.
        let masked_key: Vec<u8> = {
            let sealed = fs::read(vault(&home).join("identity.age")).unwrap();
            let key = crate::crypto::decrypt_with_passphrase(&sealed, &pass()).unwrap();
            assert!(key.starts_with(b"AGE-SECRET-KEY-1"));
            key.iter().map(|b| b ^ MASK).collect()
        };
        if let Some(at) = in_memory(&masked_key) {
            panic!("the vault key's text left in memory: {at}");
        }
        drop(dirs);
    }

    /// Phase 5 acceptance (docs/development/testing.md, "Canary scan"):
    /// runs `canary_child_lock_leaves_no_note_text` in a fresh process of
    /// this test binary, alone: dumpable (no hardening test runs there),
    /// so it can read its own /proc/self/mem.
    #[test]
    fn no_note_text_in_memory_after_lock() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::tests::canary_child_lock_leaves_no_note_text",
            ])
            .args(["--ignored", "--test-threads=1", "--nocapture"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
    }
}
