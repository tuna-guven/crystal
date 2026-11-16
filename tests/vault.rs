//! Acceptance through the real binary: unlock (startup steps 10 and 11), the tamper
//! suite, and the vault commands (`init`, `rm`).
//! Each test makes its own vault with `crystal init` in a fresh HOME,
//! damages one thing, and checks that Crystal refuses it with a fixed
//! message that names no path.
//!
//! Like tests/startup.rs, these need RLIMIT_MEMLOCK at the floor.

mod common;

use common::*;
use std::path::{Path, PathBuf};

/// A HOME with one vault, `diary`, made by `crystal init` with `PASS`.
fn home_with_vault(test: &str) -> (PathBuf, PathBuf) {
    let home = private_home(test);
    init(&home, "diary", PASS);
    let vault = vault_path(&home, "diary");
    (home, vault)
}

fn vault_path(home: &Path, name: &str) -> PathBuf {
    home.join(".local/share/crystal").join(name)
}

/// Runs `crystal init NAME` in `home` with passphrase `pass`; returns the
/// output.
fn init(home: &Path, name: &str, pass: &str) -> String {
    let mut steps = init_steps();
    if pass != PASS {
        steps[0].1 = format!("{pass}\r").into_bytes();
        steps[1].1 = format!("{pass}\r").into_bytes();
    }
    let (ok, output) = dialog(&format!("'{BIN}' init {name}"), home, &steps);
    assert!(ok, "init {name}: {output}");
    output
}

fn pass_step(pass: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![(PROMPT, format!("{pass}\r").into_bytes())]
}

/// Unlocks `name` with `pass` and quits the shell.
fn unlock_ok(home: &Path, name: &str, pass: &str) -> String {
    let prompt = format!("crystal:{name}> ");
    let mut steps = pass_step(pass);
    steps.push((prompt.as_str(), b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock {name}"), home, &steps);
    assert!(ok, "{output}");
    visible(&output)
}

/// Tries to unlock `diary` with `PASS`; it must be refused with a message
/// containing `expected`, and never show HOME's path or the passphrase.
fn assert_refused(home: &Path, expected: &str) {
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), home, &pass_step(PASS));
    assert!(!ok, "unlocked: {output}");
    assert!(output.contains(expected), "want {expected:?}: {output}");
    assert!(!output.contains("notes unlocked"), "{output}");
    assert_no_leak(home, &output);
}

/// Refused before any prompt, with a message containing `expected`.
fn assert_refused_before_prompt(home: &Path, expected: &str) {
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), home, &[]);
    assert!(!ok, "unlocked: {output}");
    assert!(!output.contains(PROMPT), "prompted: {output}");
    assert!(output.contains(expected), "want {expected:?}: {output}");
    assert_no_leak(home, &output);
}

/// No refusal names a path, the passphrase or the vault name.
fn assert_no_leak(home: &Path, output: &str) {
    let home = home.to_str().unwrap();
    assert!(!output.contains(home), "path shown: {output}");
    assert!(!output.contains(PASS), "passphrase shown: {output}");
    assert!(!output.contains("diary"), "name shown: {output}");
}

fn flip_last_byte(file: &Path) {
    let mut bytes = std::fs::read(file).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(file, bytes).unwrap();
}

fn truncate(file: &Path) {
    let bytes = std::fs::read(file).unwrap();
    std::fs::write(file, &bytes[..bytes.len() - 1]).unwrap();
}

fn state_dir(home: &Path) -> PathBuf {
    home.join(".local/state/crystal")
}

/// The vault's `<vault_id hex>.gen` in the state directory, found through
/// its pin.
fn vault_counter(home: &Path, name: &str) -> PathBuf {
    let pin = std::fs::read_to_string(state_dir(home).join(format!("{name}.pin"))).unwrap();
    state_dir(home).join(format!("{}.gen", pin.trim_end()))
}

/// The state directory's file names, sorted.
fn state_files(home: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(state_dir(home))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    files
}

// ---- unlock and the tamper suite ----

#[test]
fn init_then_unlock() {
    let (home, _) = home_with_vault("unlock_ok");
    let output = unlock_ok(&home, "diary", PASS);
    assert!(output.contains("0 notes unlocked"), "{output}");
    assert!(!output.contains(PASS), "{output}");
    // A second unlock works the same.
    unlock_ok(&home, "diary", PASS);
}

#[test]
fn refuses_wrong_passphrase() {
    let (home, _) = home_with_vault("wrong_pass");
    let (ok, output) = dialog(
        &format!("'{BIN}' unlock diary"),
        &home,
        &pass_step("not-the-passphrase"),
    );
    assert!(
        !ok && output.contains("identity.age does not open with this passphrase"),
        "{output}"
    );
    assert!(!output.contains("not-the-passphrase"), "{output}");
}

#[test]
fn refuses_missing_identity() {
    let (home, vault) = home_with_vault("no_identity");
    std::fs::remove_file(vault.join("identity.age")).unwrap();
    assert_refused(&home, "the vault has no identity.age");
}

#[test]
fn refuses_truncated_vault() {
    let (home, vault) = home_with_vault("vault_truncated");
    truncate(&vault.join("vault.age"));
    assert_refused(&home, "vault.age: the encrypted file is damaged");
}

#[test]
fn refuses_bit_flipped_vault() {
    let (home, vault) = home_with_vault("vault_flipped");
    flip_last_byte(&vault.join("vault.age"));
    assert_refused(&home, "vault.age: the encrypted file is damaged");
}

/// An older vault.age than this device has seen (the counter is ahead
/// of the file): restored from a backup, or put back by someone else.
/// Crystal explains and asks; anything but `restore` changes nothing.
#[test]
fn older_vault_needs_restore() {
    let (home, _) = home_with_vault("vault_rollback");
    let counter = vault_counter(&home, "diary");
    std::fs::write(&counter, b"2\n").unwrap();
    let question = "Type restore to accept: ";

    let mut steps = pass_step(PASS);
    steps.push((question, b"yes\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(
        !ok && output.contains("not restored; nothing was changed"),
        "{output}"
    );
    assert!(
        output.contains(
            "older than the newest version this device has seen (generation 1, last seen 2)"
        ),
        "{output}"
    );
    assert!(output.contains("do not type restore"), "{output}");
    assert!(!output.contains("notes unlocked"), "{output}");
    assert_no_leak(&home, &output);
    assert_eq!(std::fs::read(&counter).unwrap(), b"2\n");

    let mut steps = pass_step(PASS);
    steps.push((question, b"restore\r".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(
        ok && visible(&output).contains("The restored vault was accepted"),
        "{output}"
    );
    assert_eq!(std::fs::read(&counter).unwrap(), b"1\n");
    let output = unlock_ok(&home, "diary", PASS);
    assert!(!output.contains("older than"), "{output}");
}

/// The `restore` question waits with the decrypted vault in memory, so it
/// has auto-lock's limit (decision 0048), shortened to 3 seconds here:
/// with no key for that long, Crystal quits, accepting nothing. Each key
/// restarts the time: `resto`, one key a second (about 4 seconds in all),
/// is echoed whole before the timeout; without the restart, Crystal would
/// quit mid-word. The next unlock asks again.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn restore_question_times_out() {
    let (home, _) = home_with_vault("restore_timeout");
    let counter = vault_counter(&home, "diary");
    std::fs::write(&counter, b"2\n").unwrap();
    let question = "Type restore to accept: ";
    let timed_out = "no answer in time: Crystal quit, wiping the vault from memory; \
                     nothing was changed";

    let mut steps = pass_step(PASS);
    steps.push((question, b"r".to_vec()));
    for key in [b"e", b"s", b"t", b"o"] {
        steps.push(("", PAUSE_ONE_SECOND.to_vec()));
        steps.push(("", key.to_vec()));
    }
    steps.push((timed_out, Vec::new()));
    let cmd = format!("'{BIN}' unlock --lock-after-for-test 3 diary");
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(!ok, "{output}");
    let typed = output.find("resto").expect("every key was echoed");
    assert!(typed < output.find(timed_out).unwrap(), "{output}");
    assert!(!output.contains("notes unlocked"), "{output}");
    assert_no_leak(&home, &output);
    assert_eq!(std::fs::read(&counter).unwrap(), b"2\n");

    // Nothing was accepted: the question comes again.
    let mut steps = pass_step(PASS);
    steps.push((question, b"no\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(
        !ok && output.contains("not restored; nothing was changed"),
        "{output}"
    );
    assert_eq!(std::fs::read(&counter).unwrap(), b"2\n");
}

/// Another of your vaults moved in under this name: it decrypts with the
/// same passphrase, but it is not the vault pinned to the name.
#[test]
fn refuses_swapped_vault() {
    let (home, vault) = home_with_vault("vault_swapped");
    init(&home, "other", PASS);
    std::fs::rename(&vault, vault_path(&home, "original")).unwrap();
    std::fs::rename(vault_path(&home, "other"), &vault).unwrap();
    assert_refused(
        &home,
        "not the vault this name was pinned to on this device",
    );
}

/// A vault with a Syncthing conflict copy is refused; Crystal never merges
/// (decision 0045).
#[test]
fn refuses_sync_conflict() {
    let (home, vault) = home_with_vault("sync_conflict");
    std::fs::copy(
        vault.join("vault.age"),
        vault.join("vault.sync-conflict-20261106-120000-ABCDEFG.age"),
    )
    .unwrap();
    assert_refused_before_prompt(&home, "sync conflict copy");
}

#[test]
fn refuses_unknown_name_before_prompting() {
    let (home, _) = home_with_vault("unknown_name");
    let (ok, output) = dialog(&format!("'{BIN}' unlock work"), &home, &[]);
    assert!(
        !ok && output.contains("there is no vault with that name"),
        "{output}"
    );
    assert!(!output.contains(PROMPT), "prompted: {output}");
}

/// The vault directory itself must be private: a loose mode is refused
/// before any prompt, and Crystal never changes it.
#[test]
fn refuses_vault_directory_with_loose_mode() {
    let (home, vault) = home_with_vault("loose_mode");
    set_mode(&vault, 0o750);
    assert_refused_before_prompt(&home, "the vault directory has mode 750, need 700");
    assert_eq!(mode(&vault), 0o750);
}

/// The scrypt log N in a passphrase-encrypted age file's header.
fn scrypt_log_n(file: &Path) -> u8 {
    let bytes = std::fs::read(file).unwrap();
    let header = String::from_utf8_lossy(&bytes);
    let stanza = header
        .lines()
        .find(|l| l.starts_with("-> scrypt "))
        .unwrap();
    stanza.split(' ').nth(3).unwrap().parse().unwrap()
}

/// decision 0027: every passphrase file is written
/// at log N 18, whatever this device's memlock limit or speed, so every
/// device at the floor opens it.
#[test]
fn passphrase_files_use_log_n_18() {
    let (_, vault) = home_with_vault("scrypt_factor");
    assert_eq!(scrypt_log_n(&vault.join("identity.age")), 18);
}

/// An identity.age asking for log N 19 (512 MiB of scrypt) on a device at
/// the 512 MiB floor, where only 256 MiB may go to scrypt: refused before
/// any scrypt work with a message, never an abort. The work factor is
/// changed in the header text; it is read before any key is derived or
/// the header MAC is checked, so this is the path a real log N 19 file
/// takes.
#[test]
fn refuses_work_factor_beyond_locked_memory() {
    let (home, vault) = home_with_vault("scrypt_too_high");
    let identity = vault.join("identity.age");
    let mut bytes = std::fs::read(&identity).unwrap();
    let line = bytes.windows(10).position(|w| w == b"-> scrypt ").unwrap();
    let end = line + bytes[line..].iter().position(|&b| b == b'\n').unwrap();
    assert_eq!(&bytes[end - 3..end], b" 18");
    bytes[end - 1] = b'9';
    std::fs::write(&identity, bytes).unwrap();
    assert_eq!(scrypt_log_n(&identity), 19);

    let (ok, output) = dialog(
        &format!("prlimit --memlock=536870912:536870912 '{BIN}' unlock diary"),
        &home,
        &pass_step(PASS),
    );
    assert!(!ok, "unlocked: {output}");
    assert!(
        output.contains("needs more scrypt work or memory than Crystal allows here"),
        "{output}"
    );
    assert_no_leak(&home, &output);
}

/// Replaces `file` with a FIFO. Crystal must refuse it, never block on it.
fn make_fifo(file: &Path) {
    let _ = std::fs::remove_file(file);
    let ok = std::process::Command::new("mkfifo")
        .arg(file)
        .status()
        .unwrap()
        .success();
    assert!(ok, "mkfifo");
}

/// Replaces `file` with a symlink to a copy of itself next to it.
fn make_symlink(file: &Path) {
    let target = file.with_extension("real");
    std::fs::rename(file, &target).unwrap();
    std::os::unix::fs::symlink(&target, file).unwrap();
}

#[test]
fn refuses_bit_flipped_identity() {
    let (home, vault) = home_with_vault("identity_flipped");
    flip_last_byte(&vault.join("identity.age"));
    assert_refused(&home, "identity.age: the encrypted file is damaged");
}

#[test]
fn refuses_truncated_identity() {
    let (home, vault) = home_with_vault("identity_truncated");
    truncate(&vault.join("identity.age"));
    assert_refused(&home, "identity.age: the encrypted file is damaged");
}

/// identity.age from another vault under the same passphrase: it opens,
/// but vault.age is not encrypted to that key.
#[test]
fn refuses_identity_of_another_vault() {
    let (home, vault) = home_with_vault("identity_swapped");
    init(&home, "other", PASS);
    std::fs::copy(
        vault_path(&home, "other").join("identity.age"),
        vault.join("identity.age"),
    )
    .unwrap();
    assert_refused(&home, "vault.age: the file is not encrypted to this key");
}

#[test]
fn refuses_oversized_identity() {
    let (home, vault) = home_with_vault("identity_too_large");
    std::fs::write(vault.join("identity.age"), vec![b'x'; (64 << 10) + 1]).unwrap();
    assert_refused(&home, "identity.age is too large");
}

#[test]
fn refuses_identity_that_is_not_a_regular_file() {
    let (home, vault) = home_with_vault("identity_fifo");
    make_fifo(&vault.join("identity.age"));
    assert_refused(&home, "cannot read a vault file (not a regular file)");
    let (home, vault) = home_with_vault("identity_symlink");
    make_symlink(&vault.join("identity.age"));
    assert_refused(&home, "cannot read a vault file (not a regular file)");
}

#[test]
fn refuses_missing_vault_file() {
    let (home, vault) = home_with_vault("no_vault_file");
    std::fs::remove_file(vault.join("vault.age")).unwrap();
    assert_refused(&home, "the vault has no vault.age");
}

#[test]
fn refuses_vault_file_that_is_not_a_regular_file() {
    let (home, vault) = home_with_vault("vault_fifo");
    make_fifo(&vault.join("vault.age"));
    assert_refused(&home, "cannot read a vault file (not a regular file)");
}

/// VERSION is checked before any prompt: missing, a symlink, a FIFO,
/// malformed, or a format this build does not know.
#[test]
fn refuses_bad_version() {
    type Case = (&'static str, fn(&Path), &'static str);
    let cases: [Case; 5] = [
        (
            "version_missing",
            |f| std::fs::remove_file(f).unwrap(),
            "the vault has no VERSION file",
        ),
        (
            "version_symlink",
            make_symlink,
            "the vault's VERSION is not a regular file",
        ),
        (
            "version_fifo",
            make_fifo,
            "the vault's VERSION is not a regular file",
        ),
        (
            "version_malformed",
            |f| std::fs::write(f, b"one\n").unwrap(),
            "the vault's VERSION is malformed",
        ),
        (
            "version_unknown",
            |f| std::fs::write(f, b"3\n").unwrap(),
            "a format this version of Crystal cannot read",
        ),
    ];
    for (test, damage, expected) in cases {
        let (home, vault) = home_with_vault(test);
        damage(&vault.join("VERSION"));
        assert_refused_before_prompt(&home, expected);
    }
}

/// VERSION names the newest blob format the directory may hold: a vault
/// written in format 2 under a VERSION of 1 is refused after decryption.
#[test]
fn refuses_vault_newer_than_its_version() {
    let (home, vault) = home_with_vault("version_older_than_blob");
    std::fs::write(vault.join("VERSION"), b"1\n").unwrap();
    assert_refused(
        &home,
        "the vault's contents are in a newer format than its VERSION file allows",
    );
}

#[test]
fn refuses_corrupt_vault_counter() {
    let (home, _) = home_with_vault("counter_corrupt");
    std::fs::write(vault_counter(&home, "diary"), b"two\n").unwrap();
    assert_refused(
        &home,
        "the vault's generation file in ~/.local/state/crystal is malformed",
    );
}

#[test]
fn refuses_corrupt_pin() {
    let (home, _) = home_with_vault("pin_corrupt");
    std::fs::write(state_dir(&home).join("diary.pin"), b"not hex\n").unwrap();
    assert_refused(
        &home,
        "the vault's .pin file in ~/.local/state/crystal is malformed",
    );
}

/// The vault's pin and counter are gone (a new device, or a wiped state
/// directory): the unlock works, says once that the vault is pinned now
/// and rollback detection starts, and records both so the next unlock
/// says nothing.
#[test]
fn first_unlock_on_a_device_says_so_once() {
    let (home, _) = home_with_vault("first_use");
    let counter = vault_counter(&home, "diary");
    let pin = state_dir(&home).join("diary.pin");
    let pinned = std::fs::read(&pin).unwrap();
    std::fs::remove_file(&counter).unwrap();
    std::fs::remove_file(&pin).unwrap();
    let first = "First unlock of this vault on this device";
    let output = unlock_ok(&home, "diary", PASS);
    assert!(output.contains(first), "{output}");
    assert_eq!(std::fs::read(&counter).unwrap(), b"1\n");
    assert_eq!(std::fs::read(&pin).unwrap(), pinned);
    let output = unlock_ok(&home, "diary", PASS);
    assert!(!output.contains(first), "{output}");
}

/// A vault synced or restored onto a new device (another HOME) opens
/// there with its passphrase, and is pinned on its first unlock.
#[test]
fn a_vault_copied_to_another_device_opens_there() {
    let (first, vault) = home_with_vault("copy_a");
    let home = private_home("copy_b");
    let data = home.join(".local/share/crystal");
    std::fs::create_dir_all(&data).unwrap();
    for dir in [".local", ".local/share", ".local/share/crystal"] {
        set_mode(&home.join(dir), 0o700);
    }
    let copy = data.join("diary");
    std::fs::create_dir(&copy).unwrap();
    set_mode(&copy, 0o700);
    for file in std::fs::read_dir(&vault).unwrap() {
        let file = file.unwrap();
        std::fs::copy(file.path(), copy.join(file.file_name())).unwrap();
    }
    let output = unlock_ok(&home, "diary", PASS);
    assert!(output.contains("First unlock of this vault"), "{output}");
    assert_eq!(
        std::fs::read(state_dir(&home).join("diary.pin")).unwrap(),
        std::fs::read(state_dir(&first).join("diary.pin")).unwrap()
    );
}

// ---- crystal init ----

/// The passphrase is typed twice; a mismatch creates nothing and shows
/// neither entry.
#[test]
fn init_refuses_mismatched_new_passphrase() {
    let home = private_home("init_mismatch");
    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, b"correct-horse-batterz\r".to_vec()),
    ];
    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &steps);
    assert!(
        !ok && output.contains("the passphrases do not match"),
        "{output}"
    );
    assert!(
        !output.contains(PASS) && !output.contains("batterz"),
        "{output}"
    );
    assert!(!vault_path(&home, "diary").exists());
}

/// init asks for the passphrase twice and nothing else: there is no
/// recovery code (roadmap K0), and no `identity.backup.age` is written.
/// The new vault unlocks.
#[test]
fn init_asks_only_for_the_passphrase() {
    let home = private_home("init_passphrase_only");
    let output = init(&home, "diary", PASS);
    assert!(
        output.contains(
            "Crystal recommends a passphrase of 8 or more random words, generated with \
             KeePassDX or KeePassXC."
        ),
        "{output}"
    );
    assert!(!output.contains("ecovery"), "{output}");
    assert!(
        !vault_path(&home, "diary")
            .join("identity.backup.age")
            .exists()
    );
    unlock_ok(&home, "diary", PASS);
}

/// decision 0028: init refuses a new passphrase under 20 characters,
/// before the repeat, and creates nothing.
#[test]
fn new_passphrase_needs_twenty_characters() {
    let short = (NEW_PROMPT, b"nineteen-characters\r".to_vec());
    let home = private_home("init_short_pass");
    let (ok, output) = dialog(
        &format!("'{BIN}' init diary"),
        &home,
        std::slice::from_ref(&short),
    );
    assert!(!ok && output.contains("at least 20 characters"), "{output}");
    assert!(!output.contains(REPEAT), "{output}");
    assert!(!vault_path(&home, "diary").exists());
}

/// Each vault has its own passphrase.
#[test]
fn vaults_have_their_own_passphrases() {
    let (home, _) = home_with_vault("own_passphrases");
    let other = "a-second-vault-passphrase";
    init(&home, "work", other);
    unlock_ok(&home, "work", other);
    unlock_ok(&home, "diary", PASS);
    let (ok, output) = dialog(&format!("'{BIN}' unlock work"), &home, &pass_step(PASS));
    assert!(
        !ok && output.contains("does not open with this passphrase"),
        "{output}"
    );
}

// ---- crystal passwd ----

const NEW_PASS: &str = "a-new-passphrase-for-diary";
const PASSWD_DONE: &str = "The passphrase and the vault key were changed.";

/// `crystal passwd diary`: the current passphrase `old`, then `new` twice.
fn passwd(home: &Path, old: &str, new: &str) -> (bool, String) {
    let steps = vec![
        (PROMPT, format!("{old}\r").into_bytes()),
        (NEW_PROMPT, format!("{new}\r").into_bytes()),
        (REPEAT, format!("{new}\r").into_bytes()),
    ];
    dialog(&format!("'{BIN}' passwd diary"), home, &steps)
}

/// The main story (decision 0065): the new passphrase unlocks the vault,
/// the old one does not, and the notes are still there. Neither
/// passphrase is shown. identity.age and vault.age both change.
#[test]
fn passwd_changes_the_passphrase() {
    let (home, vault) = home_with_vault("passwd");
    let identity = std::fs::read(vault.join("identity.age")).unwrap();
    let vault_age = std::fs::read(vault.join("vault.age")).unwrap();
    let (ok, output) = passwd(&home, PASS, NEW_PASS);
    assert!(ok && output.contains(PASSWD_DONE), "{output}");
    assert!(
        !output.contains(PASS) && !output.contains(NEW_PASS),
        "{output}"
    );
    assert_ne!(std::fs::read(vault.join("identity.age")).unwrap(), identity);
    assert_ne!(std::fs::read(vault.join("vault.age")).unwrap(), vault_age);
    // Two writes of vault.age: generation 1 + 2.
    assert_eq!(
        std::fs::read(vault_counter(&home, "diary")).unwrap(),
        b"3\n"
    );
    unlock_ok(&home, "diary", NEW_PASS);
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &pass_step(PASS));
    assert!(
        !ok && output.contains("does not open with this passphrase"),
        "{output}"
    );
    assert_eq!(
        vault_files(&vault),
        ["VERSION", "identity.age", "vault.age"]
    );
}

/// A wrong current passphrase, a short new one, or a cancel at any prompt
/// changes nothing.
#[test]
fn passwd_refusals_change_nothing() {
    let (home, vault) = home_with_vault("passwd_refused");
    let before = (
        std::fs::read(vault.join("identity.age")).unwrap(),
        std::fs::read(vault.join("vault.age")).unwrap(),
    );
    let unchanged = || {
        assert_eq!(std::fs::read(vault.join("identity.age")).unwrap(), before.0);
        assert_eq!(std::fs::read(vault.join("vault.age")).unwrap(), before.1);
    };

    let (ok, output) = passwd(&home, "not-the-passphrase", NEW_PASS);
    assert!(
        !ok && output.contains("does not open with this passphrase"),
        "{output}"
    );
    unchanged();

    let steps = vec![
        (PROMPT, format!("{PASS}\r").into_bytes()),
        (NEW_PROMPT, b"nineteen-characters\r".to_vec()),
    ];
    let (ok, output) = dialog(&format!("'{BIN}' passwd diary"), &home, &steps);
    assert!(!ok && output.contains("at least 20 characters"), "{output}");
    unchanged();

    let all = [
        (PROMPT, format!("{PASS}\r").into_bytes()),
        (NEW_PROMPT, format!("{NEW_PASS}\r").into_bytes()),
        (REPEAT, format!("{NEW_PASS}\r").into_bytes()),
    ];
    for cut in 0..all.len() {
        let mut steps = all[..cut].to_vec();
        steps.push((all[cut].0, b"abc\x03".to_vec()));
        let (ok, output) = dialog(&format!("'{BIN}' passwd diary"), &home, &steps);
        assert!(!ok && output.contains("cancelled"), "step {cut}: {output}");
        unchanged();
    }
    unlock_ok(&home, "diary", PASS);
}

/// An older vault than this device has seen: passwd asks for `restore`
/// first, as unlock does; anything else changes nothing.
#[test]
fn passwd_asks_before_accepting_an_older_vault() {
    let (home, vault) = home_with_vault("passwd_older");
    let counter = vault_counter(&home, "diary");
    std::fs::write(&counter, b"5\n").unwrap();
    let identity = std::fs::read(vault.join("identity.age")).unwrap();
    let mut steps = vec![
        (PROMPT, format!("{PASS}\r").into_bytes()),
        (NEW_PROMPT, format!("{NEW_PASS}\r").into_bytes()),
        (REPEAT, format!("{NEW_PASS}\r").into_bytes()),
    ];
    steps.push(("Type restore to accept: ", b"no\r".to_vec()));
    let (ok, _) = dialog(&format!("'{BIN}' passwd diary"), &home, &steps);
    assert!(!ok);
    assert_eq!(std::fs::read(vault.join("identity.age")).unwrap(), identity);
    assert_eq!(std::fs::read(&counter).unwrap(), b"5\n");

    steps.pop();
    steps.push(("Type restore to accept: ", b"restore\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' passwd diary"), &home, &steps);
    assert!(ok && output.contains(PASSWD_DONE), "{output}");
    assert_eq!(std::fs::read(&counter).unwrap(), b"3\n");
    unlock_ok(&home, "diary", NEW_PASS);
}

// ---- crystal rm ----

/// rm deletes one vault; the others still unlock, and the deleted name is
/// unknown afterwards.
#[test]
fn rm_keeps_the_other_vaults() {
    let (home, vault) = home_with_vault("rm_others");
    init(&home, "other", PASS);
    let steps = vec![(RM_CONFIRM, b"diary\r".to_vec())];
    let (ok, output) = dialog(&format!("'{BIN}' rm diary"), &home, &steps);
    assert!(ok && output.contains("The vault was deleted."), "{output}");
    assert!(!vault.exists());
    assert!(state_files(&home).contains(&"other.pin".to_owned()));
    assert!(!state_files(&home).contains(&"diary.pin".to_owned()));
    unlock_ok(&home, "other", PASS);
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &[]);
    assert!(
        !ok && output.contains("there is no vault with that name"),
        "{output}"
    );
}

/// A cancelled rm (Ctrl+C at the confirmation) deletes nothing.
#[test]
fn rm_cancelled_deletes_nothing() {
    let (home, vault) = home_with_vault("rm_cancel");
    let steps = vec![(RM_CONFIRM, b"dia\x03".to_vec())];
    let (ok, output) = dialog(&format!("'{BIN}' rm diary"), &home, &steps);
    assert!(!ok && output.contains("cancelled"), "{output}");
    assert_eq!(std::fs::read_dir(&vault).unwrap().count(), 3);
    unlock_ok(&home, "diary", PASS);
}

// ---- the Crystal shell ----

/// The prompt: drawn in one styled run, so it shows up whole in the
/// output (unstyled spaces on a blank screen are not redrawn, so plain
/// text arrives in pieces).
const SHELL_READY: &str = "crystal:diary> ";

/// What the terminal shows after Crystal left the alternate screen: must
/// hold nothing from the shell.
fn after_leaving(output: &str) -> &str {
    output.rfind("\x1b[?1049l").map_or("", |at| &output[at..])
}

/// The shell answers help and unknown commands, quits, and leaves nothing
/// behind on the normal screen.
#[test]
fn shell_help_unknown_and_quit() {
    let (home, _) = home_with_vault("shell_basic");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"help\r".to_vec()));
    steps.push(("Commands:", b"frobnicate now\r".to_vec()));
    steps.push(("unknown command: frobnicate", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(output.contains("crystal:diary>"), "{output}");
    let after = after_leaving(&output);
    for gone in ["crystal:diary", "Commands", "frobnicate", "unlocked"] {
        assert!(!after.contains(gone), "{gone} left on screen: {after:?}");
    }
    assert!(output.contains("\x1b[?2004h"), "bracketed paste on");
    assert!(
        after.contains("\x1b[?2004l") || output.contains("\x1b[?2004l\x1b[?25h"),
        "and off again"
    );
}

/// `lock` (decision 0048) ends the session and asks for the passphrase
/// again in the same process. After unlocking, a new shell starts: what
/// the old one showed (`help`) is gone, and the notes are back.
#[test]
fn lock_asks_again_and_starts_a_new_shell() {
    let (home, _) = home_with_vault("lock");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    // Enter alone: Markdown (decision 0038).
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", b"# Plan\x13".to_vec()));
    steps.push(("Saved.", b"\x11".to_vec()));
    steps.push((SHELL_READY, b"help\r".to_vec()));
    steps.push(("Commands:", b"lock\r".to_vec()));
    steps.push((PROMPT, format!("{PASS}\r").into_bytes()));
    steps.push(("unlocked.", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    let relocked = output.rfind(PROMPT).unwrap();
    assert!(relocked > output.find("Commands:").unwrap(), "asked again");
    let after = &output[relocked..];
    assert!(after.contains("notes"), "{after}");
    assert!(after.contains("unlocked."), "{after}");
    assert!(
        !after.contains("Commands:"),
        "the old shell's output: {after}"
    );
    assert!(!after_leaving(&output).contains("Commands:"));
}

/// The lock screen (decision 0048) shows the cow; a wrong passphrase
/// there asks again instead of exiting, and the right one unlocks.
#[test]
fn lock_screen_asks_again_after_a_wrong_passphrase() {
    let (home, _) = home_with_vault("lock_wrong");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"lock\r".to_vec()));
    steps.push(("(oo)", b"wrong passphrase here\r".to_vec()));
    steps.push((
        "Wrong passphrase; try again.",
        format!("{PASS}\r").into_bytes(),
    ));
    steps.push(("unlocked.", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(output.contains("I locked your vault to protect you"));
    assert!(
        !after_leaving(&output).contains("(oo)"),
        "the cow stays off the main screen"
    );
}

/// The lock screen's balloon; the welcome screen shows the same cow
/// (decision 0058), so `(oo)` no longer tells them apart.
const LOCKED: &str = "I locked your vault";

/// The lock screen turns off the bracketed paste and the hidden cursor the
/// shell turned on: a passphrase pasted there arrives as plain keys, not
/// wrapped in `ESC [ 200 ~` (whose ESC would quit Crystal). A whole
/// passphrase arriving in one read, as a paste does, unlocks.
#[test]
fn lock_screen_turns_off_paste_mode_and_shows_the_cursor() {
    let (home, _) = home_with_vault("lock_paste");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"lock\r".to_vec()));
    steps.push((LOCKED, format!("{PASS}\r").into_bytes()));
    steps.push(("unlocked.", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    let before = &output[..output.find(LOCKED).unwrap()];
    let last = |s: &str| before.rfind(s).expect(s);
    assert!(last("\x1b[?2004l") > last("\x1b[?2004h"), "paste mode on");
    assert!(last("\x1b[?25h") > last("\x1b[?25l"), "cursor hidden");
}

/// A resize while the passphrase prompt is open leaves it at the top
/// left, under the cow, and keeps the keys typed so far: half the
/// passphrase typed at 80 columns, the rest after the window grew to 100,
/// unlocks. `stty` sets the size, the kernel sends Crystal the SIGWINCH,
/// and `RESIZED` written to the terminal marks when that happened.
#[test]
fn resize_keeps_the_prompt_and_the_keys() {
    let (home, _) = home_with_vault("prompt_resize");
    let (first, rest) = PASS.split_at(PASS.len() / 2);
    let under_the_cow = format!("||\r\n\r\n{PROMPT}");
    let steps = [
        (under_the_cow.as_str(), first.as_bytes().to_vec()),
        ("RESIZED", format!("{rest}\r").into_bytes()),
        (SHELL_READY, b"quit\r".to_vec()),
    ];
    let cmd = format!(
        "stty rows 24 cols 80; (sleep 2; stty rows 40 cols 100 </dev/tty; printf RESIZED >/dev/tty) & exec '{BIN}' unlock diary"
    );
    let (ok, output) = dialog(&cmd, &home, &steps);
    // The shell's prompt (the last step) came: the passphrase opened it.
    assert!(ok, "{output}");
    // Drawn once: a resize does not draw the prompt again.
    let before_shell = &output[..output.find(SHELL_READY).unwrap()];
    assert_eq!(before_shell.matches(PROMPT).count(), 1, "{output}");
}

/// Esc at the lock screen quits Crystal cleanly: exit status 0, the
/// terminal restored, nothing left on the main screen.
#[test]
fn esc_at_the_lock_screen_quits() {
    let (home, _) = home_with_vault("lock_esc");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"lock\r".to_vec()));
    steps.push(("Passphrase: ", b"\x1b".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(output.contains("\x1b[?1049l"), "left the alternate screen");
    assert!(!after_leaving(&output).contains("locked"), "{output}");
}

/// Auto-lock (decision 0048), shortened to 2 seconds by the debug-only
/// `--lock-after-for-test`: with no key in the shell, Crystal locks and
/// says why, and the passphrase unlocks again.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn auto_lock_after_idle_and_unlock_again() {
    let (home, _) = home_with_vault("auto_lock");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, Vec::new()));
    steps.push((
        "5 minutes idle; notes and history were wiped",
        format!("{PASS}\r").into_bytes(),
    ));
    steps.push(("unlocked.", b"quit\r".to_vec()));
    let cmd = format!("'{BIN}' unlock --lock-after-for-test 2 diary");
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(ok, "{output}");
    assert!(output.contains("(oo)"), "the lock screen: {output}");
}

/// Each key restarts the idle time (decision 0048): with auto-lock after
/// 3 seconds, `help` typed one key per second (about 4 seconds in all)
/// runs in the shell, and the lock comes only 3 seconds after the last
/// key. If keys did not count, the lock would come mid-word, the rest
/// would land on the lock screen, and `Commands:` would never show.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn keys_restart_the_idle_time() {
    let (home, _) = home_with_vault("auto_lock_keys");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"h".to_vec()));
    for key in [b"e", b"l", b"p", b"\r"] {
        steps.push(("", PAUSE_ONE_SECOND.to_vec()));
        steps.push(("", key.to_vec()));
    }
    steps.push(("Commands:", Vec::new()));
    steps.push((
        "5 minutes idle; notes and history were wiped",
        b"\x1b".to_vec(),
    ));
    let cmd = format!("'{BIN}' unlock --lock-after-for-test 3 diary");
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(ok, "{output}");
    let help = output.find("Commands:").unwrap();
    assert!(
        !output[..help].contains(LOCKED),
        "locked mid-word: {output}"
    );
}

/// Auto-lock in the editor saves unsaved edits first, and the lock screen
/// says so; after unlocking, the note holds them.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn auto_lock_saves_unsaved_edits_first() {
    let (home, _) = home_with_vault("auto_lock_save");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    // A text note, opened again below by its shown name (decision 0038).
    steps.push(("Markdown", b"t\r".to_vec()));
    steps.push(("Ctrl+S save", b"draft-line".to_vec()));
    steps.push((
        "your unsaved edits were saved first",
        format!("{PASS}\r").into_bytes(),
    ));
    steps.push(("unlocked.", b"view plan\r".to_vec()));
    steps.push(("previous", b"q".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let cmd = format!("'{BIN}' unlock --lock-after-for-test 2 diary");
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(ok, "{output}");
    // The frame writer skips a cell already showing the same letter (here
    // the `l` of the shell's `help` under it), so only the start.
    let locked = output.find("saved first").unwrap();
    assert!(output[locked..].contains("draft-"), "{output}");
}

/// When the save before auto-lock cannot happen (debug-only demo notes are
/// never saved), Crystal still locks, and the lock screen says the edits
/// are lost. Esc there quits.
#[test]
#[cfg_attr(not(debug_assertions), ignore = "uses a debug-only test hook")]
fn auto_lock_says_when_edits_are_lost() {
    let (home, _) = home_with_vault("auto_lock_lost");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit welcome\r".to_vec()));
    steps.push(("Ctrl+S save", b"x".to_vec()));
    steps.push(("Your edits could NOT be saved", b"\x1b".to_vec()));
    let cmd = format!("'{BIN}' unlock --demo-notes-for-test --lock-after-for-test 2 diary");
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(ok, "{output}");
}

/// SIGTSTP from outside (decision 0048) locks first, then stops: while
/// stopped the process is in state T, has left the alternate screen, and
/// has already saved the editor's unsaved text (`vault.age` changed),
/// which only a lock before the stop does; after SIGCONT it shows the lock
/// screen, and the passphrase unlocks.
/// Crystal runs as the shell's background job: `script` stops itself when
/// its own child stops, so that child must stay the shell. A shell
/// without job control keeps Crystal in its process group, so it still
/// reads the terminal, and `wait` returns only when Crystal exits.
#[test]
fn suspend_locks_first_then_stops() {
    let (home, _) = home_with_vault("suspend_lock");
    let cmd = format!(
        "v=.local/share/crystal/diary/vault.age; before=$(sha256sum < $v); \
         '{BIN}' unlock diary & p=$!; ( sleep 6; kill -TSTP $p; sleep 1; \
         echo state=$(cut -d' ' -f3 /proc/$p/stat); \
         [ \"$(sha256sum < $v)\" != \"$before\" ] && echo saved=yes || echo saved=no; \
         kill -CONT $p ) & wait $p"
    );
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    // Unsaved text in the editor when SIGTSTP comes.
    steps.push(("Ctrl+S save", b"draft-line".to_vec()));
    steps.push((
        "Locked when Crystal was suspended; your unsaved edits were saved first.",
        format!("{PASS}\r").into_bytes(),
    ));
    steps.push(("unlocked.", b"quit\r".to_vec()));
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(ok, "{output}");
    let stopped = output.find("state=T").expect("not stopped");
    let left = output[..stopped]
        .rfind("\x1b[?1049l")
        .expect("alternate screen not left");
    let shell = output.find(SHELL_READY).unwrap();
    assert!(shell < left, "left after the shell ran");
    assert!(
        output[stopped..].starts_with("state=T\r\nsaved=yes"),
        "not saved before the stop: {output}"
    );
    let lock_screen = output.find("Locked when Crystal was suspended").unwrap();
    assert!(
        stopped < lock_screen,
        "the lock screen comes after resuming"
    );
}

#[test]
fn shell_ctrl_d_quits() {
    let (home, _) = home_with_vault("shell_ctrl_d");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"\x04".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
}

/// A paste is text, never keys: a pasted "quit" and Enter do not quit, and
/// a pasted escape sequence is not passed to the terminal.
#[test]
fn shell_paste_is_never_a_command() {
    let (home, _) = home_with_vault("shell_paste");
    let mut steps = pass_step(PASS);
    steps.push((
        SHELL_READY,
        b"\x1b[200~quit\r\n\x1b]52;c;aGk=\x07\x1b[201~".to_vec(),
    ));
    steps.push(("]52;c;aGk=", b"\x15help\r".to_vec()));
    steps.push(("Commands:", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(
        !output.contains("\x1b]52"),
        "OSC 52 reached the terminal: {output:?}"
    );
}

/// The read-only commands answer on an empty vault (notes come with the
/// editor; the unit tests in commands.rs cover full vaults).
#[test]
fn shell_commands_on_an_empty_vault() {
    let (home, _) = home_with_vault("shell_commands");
    let mut steps = pass_step(PASS);
    // Only cells that change are redrawn, so each step waits for a word
    // the previous answer did not have in the same place.
    steps.push((SHELL_READY, b"ls\r".to_vec()));
    steps.push(("notes yet.", b"tags\r".to_vec()));
    steps.push(("tags yet.", b"search anything\r".to_vec()));
    steps.push(("found.", b"find x\r".to_vec()));
    steps.push(("matching", b"links\r".to_vec()));
    steps.push(("broken", b"view nothing\r".to_vec()));
    steps.push(("find).", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
}

// ---- view and the clipboard, on fixed notes added in memory by the
// ---- debug-only --demo-notes-for-test (one of them hostile; never saved) ----

/// Runs `crystal --demo-notes-for-test diary` through `steps` after the
/// passphrase.
fn demo(test: &str, steps: &[(&str, &[u8])]) -> String {
    let (home, _) = home_with_vault(test);
    let mut all = pass_step(PASS);
    all.extend(steps.iter().map(|(wait, keys)| (*wait, keys.to_vec())));
    let (ok, output) = dialog(
        &format!("'{BIN}' unlock --demo-notes-for-test diary"),
        &home,
        &all,
    );
    assert!(ok, "{output}");
    output
}

/// The OSC 52 sequences in `output`, in order.
fn osc52(output: &str) -> Vec<&str> {
    output
        .match_indices("\x1b]52;c;")
        .map(|(at, _)| {
            let rest = &output[at + 7..];
            &rest[..rest.find('\x07').unwrap_or(rest.len())]
        })
        .collect()
}

/// In the pager: y copies into Crystal's own clipboard, and nothing is
/// ever sent to the system clipboard (no OSC 52, decision 0041); links are
/// followed with n and Enter.
#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "uses the debug-only --demo-notes-for-test"
)]
fn pager_copies_inside_and_follows_links() {
    let output = demo(
        "pager_copy",
        &[
            (SHELL_READY, b"view welcome\r"),
            ("Welcome", b"y"),
            ("inside", b"Yn\r"),
            ("Keys", b"q"),
            (SHELL_READY, b"quit\r"),
        ],
    );
    assert!(osc52(&output).is_empty(), "OSC 52 sent: {output:?}");
    assert!(!after_leaving(&output).contains("secret-demo-text"));
}

/// A note holding an OSC 52 sequence of its own cannot set the clipboard:
/// viewing it sends no OSC 52 at all, and its escapes are spelled out.
#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "uses the debug-only --demo-notes-for-test"
)]
fn hostile_note_cannot_set_the_clipboard() {
    let output = demo(
        "hostile_view",
        &[
            (SHELL_READY, b"view hostile\r"),
            ("<U+001B>", b"q"),
            (SHELL_READY, b"quit\r"),
        ],
    );
    assert!(osc52(&output).is_empty(), "{output:?}");
    // The payload is on screen only as text, its ESC and BEL spelled out.
    let shown = visible(&output);
    assert!(shown.contains("<U+001B>]52;c;ZXZpbA==<U+0007>b"), "{shown}");
    assert!(shown.contains("<U+202E>"), "{shown}");
}

/// Clickable links through the binary (decision 0054): the one good
/// target reaches the terminal inside OSC 8 and is closed again; the
/// target with an ESC smuggled in as `&#27;` and the `file:` one never
/// do, and show their address as text.
#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "uses the debug-only --demo-notes-for-test"
)]
fn only_clickable_targets_reach_the_terminal() {
    let output = demo(
        "links_view",
        &[
            (SHELL_READY, b"view links\r"),
            ("previous", b"q"),
            (SHELL_READY, b"quit\r"),
        ],
    );
    let targets: Vec<&str> = output
        .match_indices("\x1b]8;;")
        .map(|(at, _)| {
            let rest = &output[at + 5..];
            &rest[..rest.find("\x1b\\").unwrap_or(rest.len())]
        })
        .collect();
    let opened: Vec<&str> = targets.iter().copied().filter(|t| !t.is_empty()).collect();
    assert_eq!(opened, ["https://example.org/ok"], "{output:?}");
    // The next OSC 8 after it closes it.
    assert!(
        targets
            .windows(2)
            .any(|pair| pair == ["https://example.org/ok", ""]),
        "{output:?}"
    );
    let shown = visible(&output);
    assert!(shown.contains("(file:///etc/passwd)"), "{shown}");
    assert!(
        shown.contains("<U+001B>]8;;https://evil.example<U+0007>"),
        "{shown}"
    );
}

// ---- the editor (decision 0045): new, edit, save, preview ----

/// A string only these tests type, to look for on disk.
const CANARY: &str = "canary-7f3a9c";

/// The files in the vault directory, sorted.
fn vault_files(vault: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(vault)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    files
}

/// `new` opens the editor; Ctrl+S saves the note into vault.age, where
/// only ciphertext lands; the next unlock shows it with `view`.
#[test]
fn new_note_is_saved_and_opens_again() {
    let (home, vault) = home_with_vault("editor_new");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit work/plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", format!("hello {CANARY}\x13").into_bytes()));
    steps.push(("Saved.", b"\x11".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(!after_leaving(&output).contains(CANARY));

    // Only Crystal's three files, and the text nowhere in them.
    assert_eq!(
        vault_files(&vault),
        ["VERSION", "identity.age", "vault.age"]
    );
    for file in vault_files(&vault) {
        let bytes = std::fs::read(vault.join(&file)).unwrap();
        let found = bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes());
        assert!(!found, "{file} holds the note's text");
    }

    // The frame writer skips cells that did not change, so a text can
    // arrive with gaps wherever it covers the same letter of the shell's
    // frame ("hello" over "Type", the last `c` over "commands."). Wait for
    // the pager's status line, the end of its frame, and look for the
    // canary's hex digits, which the shell's frame never shows, without
    // the last one.
    let mut steps = pass_step(PASS);
    steps.push(("unlocked.", b"view work/plan\r".to_vec()));
    steps.push(("previous", b"q".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    let hex = &CANARY["canary-".len()..CANARY.len() - 1];
    assert!(output.contains(hex), "{output}");
}

/// `edit` changes a saved note; Ctrl+E shows the unsaved text rendered and
/// returns; Ctrl+Q with unsaved changes warns before it discards them.
#[test]
fn edit_previews_saves_and_guards_unsaved_changes() {
    let (home, _) = home_with_vault("editor_edit");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", b"# Plan\x13\x11".to_vec()));
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    // End of the text, a new line, unsaved; then the preview.
    steps.push(("Ctrl+S save", b"\x1b[B\x1b[F\rdraft-line\x05".to_vec()));
    steps.push(("preview", b"\x05".to_vec()));
    // Back in the editor (its title shows unsaved changes): save, change
    // again, and try to leave.
    steps.push(("[+]", b"\x13".to_vec()));
    steps.push(("Saved.", b"!\x11".to_vec()));
    steps.push(("again", b"\x11".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");

    // Saved: the new line. Discarded: the "!" typed after the save.
    let mut steps = pass_step(PASS);
    steps.push(("unlocked.", b"view plan\r".to_vec()));
    steps.push(("draft-line", b"q".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(!visible(&output).contains("draft-line!"), "{output:?}");
}

/// A session with the debug-only demo notes never saves: vault.age stays
/// byte for byte the same.
#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "uses the debug-only --demo-notes-for-test"
)]
fn demo_notes_are_never_saved() {
    let (home, vault) = home_with_vault("editor_demo");
    let before = std::fs::read(vault.join("vault.age")).unwrap();
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit welcome\r".to_vec()));
    steps.push(("Ctrl+S save", b"x\x13".to_vec()));
    steps.push(("never saved", b"\x11\x11".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(
        &format!("'{BIN}' unlock --demo-notes-for-test diary"),
        &home,
        &steps,
    );
    assert!(ok, "{output}");
    assert_eq!(std::fs::read(vault.join("vault.age")).unwrap(), before);
}

// ---- Phase 4 acceptance: kill -9 during a save never corrupts the vault ----

/// Unlocks the vault, opens `edit n`, types 300 characters each followed
/// by Ctrl+S (300 saves in a row), and kills Crystal with SIGKILL
/// `delay_ms` after the burst was sent.
fn kill_during_saves(home: &Path, delay_ms: u64) {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // The shell prints its own pid, then becomes Crystal (`exec` keeps the
    // pid): the line always comes before Crystal's prompt. With Crystal as
    // a background job printing `$!`, the prompt could come first, and the
    // wait for it, which starts after the pid line, never ended.
    let cmd = format!("echo; echo crystal-pid=$$; exec '{BIN}' unlock diary");
    let mut child = Command::new("script")
        .args(["-qec", &cmd, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .env("HOME", home)
        .current_dir(home)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
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
    let mut output = Vec::new();
    let mut seen = 0;
    let mut wait_for = |marker: &str, output: &mut Vec<u8>| {
        loop {
            if let Some(at) = output[seen..]
                .windows(marker.len())
                .position(|w| w == marker.as_bytes())
            {
                seen += at + marker.len();
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(chunk) => output.extend(chunk),
                Err(_) => panic!("no {marker:?}: {:?}", text(output)),
            }
        }
    };
    wait_for("crystal-pid=", &mut output);
    wait_for("\n", &mut output);
    let line = text(&output);
    let pid: String = line[line.rfind("crystal-pid=").unwrap() + 12..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    wait_for(PROMPT, &mut output);
    stdin.write_all(format!("{PASS}\r").as_bytes()).unwrap();
    wait_for(SHELL_READY, &mut output);
    stdin.write_all(b"edit n\r").unwrap();
    wait_for("Ctrl+S save", &mut output);
    stdin.write_all(&b"x\x13".repeat(300)).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(Duration::from_millis(delay_ms));
    let killed = Command::new("kill").args(["-9", &pid]).status().unwrap();
    assert!(killed.success(), "kill -9 {pid}");
    drop(stdin);
    let _ = child.wait();
}

/// The vault's generation counter, as stored in the state directory.
fn counter(home: &Path) -> u64 {
    let text = std::fs::read_to_string(vault_counter(home, "diary")).unwrap();
    text.trim_end().parse().unwrap()
}

/// Phase 4 acceptance: Crystal is killed with SIGKILL while saving, at a
/// different moment each round. Every time, the vault opens again without
/// a rollback warning (the counter is never ahead of the file), and only
/// Crystal's own files and atomic-writer leftovers are in the directory.
///
/// At least one kill must land mid-write, between the atomic writer's
/// temp file and its rename: the rounds go on until one leaves a temp file
/// behind (each write sweeps older ones, and the test removes them before
/// each round, so a temp file after a kill is that round's).
#[test]
fn kill_9_during_saves_never_corrupts_the_vault() {
    const MIN_ROUNDS: u64 = 12;
    const MAX_ROUNDS: u64 = 60;
    let (home, vault) = home_with_vault("kill_during_saves");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit n\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", b"start\x13".to_vec()));
    steps.push(("Saved.", b"\x11".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    let first = counter(&home);
    let is_temp = |file: &str| file.starts_with(".vault.age.tmp-");

    let mut mid_write = 0;
    let mut round = 0;
    while round < MIN_ROUNDS || (mid_write == 0 && round < MAX_ROUNDS) {
        for file in vault_files(&vault).into_iter().filter(|f| is_temp(f)) {
            std::fs::remove_file(vault.join(file)).unwrap();
        }
        // 0 to 330 ms after the burst: before, during and after saves.
        kill_during_saves(&home, (round % 12) * 30);
        let files = vault_files(&vault);
        for file in &files {
            let known = ["VERSION", "identity.age", "vault.age"];
            assert!(
                known.contains(&file.as_str()) || is_temp(file),
                "round {round}: unexpected {file}"
            );
        }
        if files.iter().any(|f| is_temp(f)) {
            mid_write += 1;
        }
        // Opens at once: no damage, and no rollback (a counter ahead of
        // the file would stop at the `restore` question instead).
        let steps = vec![
            (PROMPT, format!("{PASS}\r").into_bytes()),
            (SHELL_READY, b"quit\r".to_vec()),
        ];
        let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
        assert!(ok, "round {round}: {output}");
        assert!(!output.contains("restore"), "round {round}: {output}");
        round += 1;
    }
    assert!(
        mid_write > 0,
        "no kill in {round} rounds landed between a temp file and its rename"
    );
    // Saves ran in the rounds, not only before them.
    assert!(
        counter(&home) > first + MIN_ROUNDS,
        "too few saves happened: {} after {first}",
        counter(&home)
    );
}

// ---- Phase 4 acceptance: strace shows only ciphertext writes inside the vault ----

/// Whether strace can trace a child here. secureblue's SELinux boolean
/// `deny_ptrace` blocks it (docs/guide/setup.md, "ptrace and SELinux
/// deny_ptrace"); GitHub's runners allow it, and CI installs strace.
fn strace_works() -> bool {
    std::process::Command::new("strace")
        .args(["-f", "-o", "/dev/null", "true"])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `-xx` prints every byte as `\xHH`; this undoes it.
fn unhex(quoted: &str) -> Vec<u8> {
    quoted
        .split("\\x")
        .skip(1)
        .map(|h| u8::from_str_radix(&h[..2], 16).unwrap())
        .collect()
}

/// The path `strace -yy` printed for the first fd argument, `name(N</path>`.
/// `-xx` prints the path as `\xHH` too, so it holds no `<` or `>` of its
/// own; a device adds its numbers after it (`3</dev/tty<char 5:0>>`).
fn fd_path(call: &str) -> Option<String> {
    let start = call.find('<')? + 1;
    let end = start + call[start..].find(['<', '>'])?;
    String::from_utf8(unhex(&call[start..end])).ok()
}

/// Like `fd_path`, but a missing path fails the test: strace prints the
/// bare fd number when it cannot read `/proc/PID/fd`, and a check that
/// skipped such calls would pass without checking anything.
fn traced_path(call: &str, part: &str) -> String {
    fd_path(part).unwrap_or_else(|| {
        panic!(
            "strace printed no path for an fd ({call}): Crystal is non-dumpable, so its \
             /proc/PID/fd is root's, mode 0500; strace needs CAP_DAC_READ_SEARCH to read it, \
             as CI gives it"
        )
    })
}

/// The quoted string arguments of a call, decoded from `-xx`'s `\xHH`
/// (which also turns `"` into `\x22`, so a quote only ever delimits).
fn quoted(call: &str) -> Vec<Vec<u8>> {
    call.split('"').skip(1).step_by(2).map(unhex).collect()
}

/// A note title only the strace test types, to look for on disk.
const TITLE_CANARY: &str = "title-canary-5e1d0b";

/// Every syscall that can write, create, rename or remove a file. strace
/// traces them all; Crystal may use only some, and only as checked below.
const WRITE_FAMILY: &str = "open,openat,creat,write,writev,pwrite64,pwritev,pwritev2,\
                            sendfile,copy_file_range,splice,rename,renameat,renameat2,\
                            unlink,unlinkat,link,linkat,symlink,symlinkat,mkdir,mkdirat,\
                            truncate,ftruncate,fallocate";

/// Phase 4 acceptance: with every write-family syscall of a `new`, type,
/// Ctrl+S session traced, Crystal writes inside the vault directory only
/// the atomic writer's temp files, holding an age file and never the
/// note's title or text; in the state directory only the counter's temp
/// file, holding a number; and otherwise only to its terminal (/dev/pts).
/// Each file's bytes are checked whole, so text split across two writes
/// is still found.
///
/// Crystal is non-dumpable, so strace sees its written bytes only with
/// CAP_SYS_PTRACE (without it, it shows addresses) and its fd paths only
/// with CAP_DAC_READ_SEARCH (without it, bare fd numbers). The test
/// therefore runs in CI only, which gives strace both capabilities and
/// runs it with `--ignored`.
#[test]
#[ignore = "CI only: needs strace with CAP_SYS_PTRACE and CAP_DAC_READ_SEARCH (see .github/workflows/ci.yml)"]
fn strace_shows_only_ciphertext_written() {
    use std::collections::BTreeMap;

    assert!(
        strace_works(),
        "strace cannot trace here (not installed, or ptrace denied): this test runs in CI"
    );
    let (home, vault) = home_with_vault("strace");
    let state = state_dir(&home);
    let trace = home.join("trace.txt");
    let cmd = format!(
        "strace -f -qq -yy -xx -s 100000000 -e trace={WRITE_FAMILY} -o '{}' '{BIN}' unlock diary",
        trace.display()
    );
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, format!("edit {TITLE_CANARY}\r").into_bytes()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", format!("{CANARY}\x13").into_bytes()));
    steps.push(("Saved.", b"\x11".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&cmd, &home, &steps);
    assert!(ok, "{output}");

    let vault = vault.to_str().unwrap();
    let state = state.to_str().unwrap();
    let temp_in = |path: &str, dir: &str, name: &str| {
        path.strip_prefix(dir)
            .and_then(|rest| rest.strip_prefix("/."))
            .is_some_and(|file| file.starts_with(name) && file[name.len()..].starts_with(".tmp-"))
    };
    // Crystal opens /dev/tty (startup step 5); the pty behind it is /dev/pts.
    let terminal = |path: &str| path == "/dev/tty" || path.starts_with("/dev/pts/");
    // What was written to each file, in order.
    let mut written: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let log = std::fs::read_to_string(&trace).unwrap();
    for line in log.lines() {
        // `-f` puts the pid first.
        let call = line
            .split_once(' ')
            .map_or(line, |(_, call)| call.trim_start());
        // Signals (`---`), exits (`+++`) and anything not a call.
        let Some((name, _)) = call.split_once('(') else {
            continue;
        };
        if !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            continue;
        }
        match name {
            "open" | "openat" => {
                let writes = ["O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND"];
                if !writes.iter().any(|flag| call.contains(flag)) {
                    continue;
                }
                let Some((_, result)) = call.rsplit_once(" = ") else {
                    continue;
                };
                if result.starts_with('-') {
                    continue; // failed open
                }
                let path = traced_path(call, result);
                let path = path.as_str();
                assert!(
                    terminal(path)
                        || temp_in(path, vault, "vault.age")
                        || temp_in(path, vault, "VERSION")
                        || (path.starts_with(state) && path.contains(".gen.tmp-")),
                    "opened for writing: {call}"
                );
            }
            "write" | "writev" | "pwrite64" | "pwritev" | "pwritev2" => {
                let path = traced_path(call, call);
                if terminal(&path) {
                    continue;
                }
                assert_eq!(name, "write", "only write() reaches a file: {call}");
                // Without CAP_SYS_PTRACE, strace cannot read a non-dumpable
                // process and prints an address instead of the bytes.
                let Some(quote) = call.find('"') else {
                    panic!(
                        "strace could not read what Crystal wrote ({call}): it needs \
                         CAP_SYS_PTRACE, as CI gives it"
                    );
                };
                written
                    .entry(path)
                    .or_default()
                    .extend(unhex(&call[quote..]));
            }
            // A temp file onto its target, in the same directory: VERSION
            // and vault.age in the vault, a counter in the state directory.
            "renameat2" => {
                let dir = traced_path(call, call);
                let [from, to] = &quoted(call)[..] else {
                    panic!("renamed: {call}");
                };
                let to = String::from_utf8_lossy(to);
                let allowed = if dir == vault {
                    to == "vault.age" || to == "VERSION"
                } else {
                    dir == state && to.ends_with(".gen")
                };
                assert!(
                    allowed && from.starts_with(format!(".{to}.tmp-").as_bytes()),
                    "renamed: {call}"
                );
            }
            "unlinkat" => {
                let names = quoted(call);
                assert!(
                    names.len() == 1 && names[0].windows(5).any(|w| w == b".tmp-"),
                    "deleted: {call}"
                );
            }
            // The state directory's walk: each level exists already.
            "mkdirat" => assert!(call.contains("= -1 EEXIST"), "created: {call}"),
            _ => panic!("unexpected write-family call: {call}"),
        }
    }

    let mut vault_files = 0;
    for (path, data) in &written {
        for canary in [CANARY, TITLE_CANARY] {
            assert!(
                !data.windows(canary.len()).any(|w| w == canary.as_bytes()),
                "note content ({canary}) written to {path}"
            );
        }
        if temp_in(path, vault, "vault.age") {
            vault_files += 1;
            assert!(
                data.starts_with(b"age-encryption.org/v1\n"),
                "not an age file: {path}"
            );
        } else if path.starts_with(state) {
            assert!(
                data.iter().all(|b| b.is_ascii_digit() || *b == b'\n'),
                "not a counter: {path}"
            );
        } else {
            assert!(temp_in(path, vault, "VERSION"), "written: {path}");
            assert_eq!(data, b"2\n");
        }
    }
    assert!(vault_files >= 1, "no save was traced: {log}");
}

// ---- mv and rm (decision 0033) ----

/// `mv` renames a note and rewrites the links to it in the same save; `rm`
/// deletes only after `y`. Both hold after a fresh unlock.
#[test]
fn mv_rewrites_links_and_rm_asks_first() {
    let (home, _) = home_with_vault("mv_rm");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", b"# Plan\x13".to_vec()));
    // Closing the editor draws the shell again, the first questions too:
    // wait past them ("later." ends the type question), so "Create"
    // below is the second note's question.
    steps.push(("Saved.", b"\x11".to_vec()));
    steps.push(("later.", b"edit index\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", b"See [[plan|the plan]].\x13\x11".to_vec()));
    steps.push((SHELL_READY, b"mv plan work/goals\r".to_vec()));
    steps.push(("Moved.", b"rm index\r".to_vec()));
    // Waits on the last word of a line: the frame writer skips cells
    // that did not change, so a line may arrive in pieces. The answer is
    // typed after the question showed (a typed-ahead one is dropped, see
    // rm_ignores_a_typed_ahead_answer); the next unlock checks the result.
    steps.push(("delete.", b"no\r".to_vec()));
    steps.push(("", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");

    // The link followed the note (n selects it, Enter follows it to
    // work/goals); then index is deleted for good.
    let mut steps = pass_step(PASS);
    steps.push(("unlocked.", b"view index\r".to_vec()));
    steps.push(("the plan", b"n\r".to_vec()));
    steps.push(("work/goals", b"q".to_vec()));
    steps.push((SHELL_READY, b"rm index\r".to_vec()));
    steps.push(("delete.", b"y\r".to_vec()));
    steps.push(("", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");

    let mut steps = pass_step(PASS);
    steps.push(("unlocked.", b"view index\r".to_vec()));
    steps.push(("No note with that name", b"view goals\r".to_vec()));
    steps.push(("Plan", b"q".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
}

/// "rm NAME⏎y⏎" arriving in one read (pasted or typed ahead) deletes
/// nothing: the answer must be typed after the question showed. A command
/// typed while rm waits still runs after "Not deleted.", and an empty
/// Enter answers like `n`.
#[test]
fn rm_ignores_a_typed_ahead_answer() {
    let (home, _) = home_with_vault("rm_typed_ahead");
    let mut steps = pass_step(PASS);
    steps.push((SHELL_READY, b"edit plan\r".to_vec()));
    steps.push(("Create", b"y\r".to_vec()));
    steps.push(("Markdown", b"\r".to_vec()));
    steps.push(("Ctrl+S save", b"# Plan\x13\x11".to_vec()));
    // One write, so one read: the `y` is dropped with the rest of it.
    steps.push((SHELL_READY, b"rm plan\ry\r".to_vec()));
    // The pager opens at once, so the shell's "Not deleted." shows after q.
    steps.push(("delete.", b"view plan\r".to_vec()));
    steps.push(("Plan", b"q".to_vec()));
    steps.push(("deleted.", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    let shown = visible(&output);
    assert!(shown.contains("Not deleted."), "{shown}");
    assert!(!shown.contains("Deleted."), "{shown}");

    // An empty Enter answers too: a later `y` is just an unknown command.
    let mut steps = pass_step(PASS);
    // "trash:" lies right of the unlock lines, so the frame writer, which
    // skips cells that did not change, draws it whole.
    steps.push(("unlocked.", b"rm plan\r".to_vec()));
    steps.push(("trash:", b"\r".to_vec()));
    steps.push(("Not", b"y\r".to_vec()));
    steps.push(("unknown", b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
    let shown = visible(&output);
    assert!(shown.contains("unknown command: y"), "{shown}");
    assert!(!shown.contains("Deleted."), "{shown}");

    // Still there after a fresh unlock.
    let mut steps = pass_step(PASS);
    steps.push(("unlocked.", b"view plan\r".to_vec()));
    steps.push(("Plan", b"q".to_vec()));
    steps.push((SHELL_READY, b"quit\r".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' unlock diary"), &home, &steps);
    assert!(ok, "{output}");
}
