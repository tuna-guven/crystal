//! Phase 2 acceptance through the real binary (docs/development/roadmap.md):
//! unlock (startup step 8) and the tamper suite. Each test makes its own
//! vault with `crystal init` in a fresh HOME, damages one thing, and checks that
//! unlock refuses it with a fixed message that names no path.
//!
//! Like tests/startup.rs, these need RLIMIT_MEMLOCK at the floor.

mod common;

use age::secrecy::ExposeSecret;
use common::*;
use std::path::{Path, PathBuf};

/// A HOME with one vault, `diary`, made by `crystal init` with `PASS`.
fn home_with_vault(test: &str) -> (PathBuf, PathBuf) {
    let home = private_home(test);
    init(&home, "diary", PASS);
    let vault = home.join("Vaults/diary");
    (home, vault)
}

/// Runs `crystal init NAME` in `home` with passphrase `pass`.
fn init(home: &Path, name: &str, pass: &str) {
    let recipient = age::x25519::Identity::generate().to_public().to_string();
    let mut steps = init_steps(&recipient);
    if pass != PASS {
        steps[0].1 = format!("{pass}\r").into_bytes();
        steps[1].1 = format!("{pass}\r").into_bytes();
    }
    // A device that already has a config asks for its passphrase once.
    if home.join(".config/crystal/config.age").exists() {
        steps = vec![
            (PROMPT, format!("{pass}\r").into_bytes()),
            steps[2].clone(),
            steps[3].clone(),
        ];
    }
    let (ok, output) = dialog(&format!("'{BIN}' init {name}"), home, &steps);
    assert!(ok, "init {name}: {output}");
}

/// Unlocks `name` with `pass` and quits at the placeholder screen.
fn unlock_ok(home: &Path, name: &str, pass: &str) -> String {
    let steps = vec![
        (PROMPT, format!("{pass}\r").into_bytes()),
        ("notes unlocked", b"q".to_vec()),
    ];
    let (ok, output) = dialog(&format!("'{BIN}' {name}"), home, &steps);
    assert!(ok, "{output}");
    output
}

/// Tries to unlock `diary` with `PASS`; it must be refused with a message
/// containing `expected`, and never show HOME's path or the passphrase.
fn assert_refused(home: &Path, expected: &str) {
    let steps = vec![(PROMPT, format!("{PASS}\r").into_bytes())];
    let (ok, output) = dialog(&format!("'{BIN}' diary"), home, &steps);
    assert!(!ok, "unlocked: {output}");
    assert!(output.contains(expected), "want {expected:?}: {output}");
    assert!(!output.contains("notes unlocked"), "{output}");
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

/// The vault's `<vault_id hex>.gen` in the state directory.
fn vault_counter(home: &Path) -> PathBuf {
    std::fs::read_dir(home.join(".local/state/crystal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap() != "config.gen")
        .unwrap()
}

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
    let steps = vec![(PROMPT, b"not-the-passphrase\r".to_vec())];
    let (ok, output) = dialog(&format!("'{BIN}' diary"), &home, &steps);
    assert!(
        !ok && output.contains("config.age: wrong passphrase"),
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

/// An older vault.age restored after this device saw a newer one: the
/// device's counter is ahead of the file.
#[test]
fn refuses_rolled_back_vault() {
    let (home, _) = home_with_vault("vault_rollback");
    std::fs::write(vault_counter(&home), b"2\n").unwrap();
    assert_refused(
        &home,
        "the vault is older than the last one this device has seen (generation 1, last seen 2)",
    );
}

/// Another vault swapped in at the registered path: it decrypts with the
/// same passphrase, but its vault_id is not the pinned one.
#[test]
fn refuses_foreign_vault() {
    let (home, vault) = home_with_vault("vault_foreign");
    init(&home, "other", PASS);
    let vaults = home.join("Vaults");
    std::fs::rename(&vault, vaults.join("diary-original")).unwrap();
    std::fs::rename(vaults.join("other"), &vault).unwrap();
    assert_refused(&home, "holds a different vault");
}

#[test]
fn refuses_truncated_config() {
    let (home, _) = home_with_vault("config_truncated");
    truncate(&home.join(".config/crystal/config.age"));
    assert_refused(&home, "config.age: the encrypted file is damaged");
}

#[test]
fn refuses_bit_flipped_config() {
    let (home, _) = home_with_vault("config_flipped");
    flip_last_byte(&home.join(".config/crystal/config.age"));
    assert_refused(&home, "config.age: the encrypted file is damaged");
}

#[test]
fn refuses_rolled_back_config() {
    let (home, _) = home_with_vault("config_rollback");
    std::fs::write(home.join(".local/state/crystal/config.gen"), b"5\n").unwrap();
    assert_refused(
        &home,
        "the config is older than the last one this device has seen (generation 1, last seen 5)",
    );
}

/// A config.age from elsewhere, encrypted to another passphrase.
#[test]
fn refuses_foreign_config() {
    let (home, _) = home_with_vault("config_foreign");
    let elsewhere = private_home("config_foreign_other");
    init(&elsewhere, "diary", "another-passphrase-here");
    std::fs::copy(
        elsewhere.join(".config/crystal/config.age"),
        home.join(".config/crystal/config.age"),
    )
    .unwrap();
    assert_refused(&home, "config.age: wrong passphrase");
}

/// The config can point Crystal at a vault, but the path is checked as
/// attacker-written at every unlock: a group-writable parent is refused.
#[test]
fn refuses_vault_below_writable_parent() {
    let (home, _) = home_with_vault("writable_parent");
    set_mode(&home.join("Vaults"), 0o770);
    assert_refused(&home, "writable by group or others");
}

/// Phase 2 refuses a vault with a Syncthing conflict copy; #31 merges.
#[test]
fn refuses_sync_conflict() {
    let (home, vault) = home_with_vault("sync_conflict");
    std::fs::copy(
        vault.join("vault.age"),
        vault.join("vault.sync-conflict-20261106-120000-ABCDEFG.age"),
    )
    .unwrap();
    assert_refused(&home, "sync conflict copy");
}

#[test]
fn refuses_unknown_name() {
    let (home, _) = home_with_vault("unknown_name");
    let steps = vec![(PROMPT, format!("{PASS}\r").into_bytes())];
    let (ok, output) = dialog(&format!("'{BIN}' work"), &home, &steps);
    assert!(
        !ok && output.contains("no vault with that name is registered"),
        "{output}"
    );
}

/// A device with a config directory but no config.age: refused before any
/// prompt, and nothing is created.
#[test]
fn refuses_without_config_before_prompting() {
    let home = private_home("no_config");
    let config = home.join(".config/crystal");
    std::fs::create_dir_all(&config).unwrap();
    set_mode(&home.join(".config"), 0o700);
    set_mode(&config, 0o700);
    let (ok, output) = dialog(&format!("'{BIN}' diary"), &home, &[]);
    assert!(
        !ok && output.contains("no vault is registered on this device"),
        "{output}"
    );
    assert!(!output.contains(PROMPT), "prompted: {output}");
    assert!(!config.join("config.age").exists());
}

/// `crystal init NAME --at DIR` makes DIR itself the vault, and it unlocks.
#[test]
fn init_at_then_unlock() {
    let home = private_home("init_at");
    let synced = home.join("synced");
    std::fs::create_dir(&synced).unwrap();
    set_mode(&synced, 0o755);
    let recipient = age::x25519::Identity::generate().to_public().to_string();
    let target = synced.join("work");
    let (ok, output) = dialog(
        &format!("'{BIN}' init work --at '{}'", target.display()),
        &home,
        &init_steps(&recipient),
    );
    assert!(ok, "{output}");
    assert_eq!(mode(&target), 0o700);
    assert!(target.join("vault.age").exists());
    assert!(!home.join("Vaults").exists());
    let output = unlock_ok(&home, "work", PASS);
    assert!(output.contains("0 notes unlocked"), "{output}");
}

/// docs/design/storage.md: "If the config is lost, nothing is lost:
/// `crystal add NAME DIR` registers an existing vault again." config.gen
/// survives the loss (it lives in the state directory), so the new
/// config.age must start above it, not at generation 1.
#[test]
fn add_after_lost_config_registers_again() {
    let (home, vault) = home_with_vault("lost_config");
    // A second init raises config.gen to 2.
    init(&home, "other", PASS);
    std::fs::remove_file(home.join(".config/crystal/config.age")).unwrap();

    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
    ];
    let (ok, output) = dialog(
        &format!("'{BIN}' add diary '{}'", vault.display()),
        &home,
        &steps,
    );
    assert!(ok, "add after a lost config: {output}");
    let output = unlock_ok(&home, "diary", PASS);
    assert!(output.contains("0 notes unlocked"), "{output}");
}

/// Cancelling the passphrase prompt of `crystal add` or `crystal remove`
/// changes nothing: config.age stays byte for byte the same.
#[test]
fn add_and_remove_cancelled_change_nothing() {
    let (home, vault) = home_with_vault("cancel_add_remove");
    let config = home.join(".config/crystal/config.age");
    let before = std::fs::read(&config).unwrap();
    let cancel = vec![(PROMPT, b"abc\x03".to_vec())];

    let (ok, output) = dialog(&format!("'{BIN}' remove diary"), &home, &cancel);
    assert!(!ok && output.contains("cancelled"), "{output}");
    assert_eq!(std::fs::read(&config).unwrap(), before);

    let (ok, output) = dialog(
        &format!("'{BIN}' add other '{}'", vault.display()),
        &home,
        &cancel,
    );
    assert!(!ok && output.contains("cancelled"), "{output}");
    assert_eq!(std::fs::read(&config).unwrap(), before);
    unlock_ok(&home, "diary", PASS);
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

/// docs/decisions/0027-scrypt-fixed.md: every passphrase file is written
/// at log N 18, whatever this device's memlock limit or speed, so every
/// device at the floor opens it.
#[test]
fn passphrase_files_use_log_n_18() {
    let (home, vault) = home_with_vault("scrypt_factor");
    assert_eq!(scrypt_log_n(&home.join(".config/crystal/config.age")), 18);
    assert_eq!(scrypt_log_n(&vault.join("identity.age")), 18);
}

/// A config.age asking for log N 19 (512 MiB of scrypt) on a device at the
/// 512 MiB floor, where only 256 MiB may go to scrypt: refused before any
/// scrypt work with a message, never an abort.
#[test]
fn refuses_work_factor_beyond_locked_memory() {
    use age::secrecy::SecretString;
    use std::io::{Read, Write};
    let (home, _) = home_with_vault("scrypt_too_high");
    let config = home.join(".config/crystal/config.age");
    let pass = || SecretString::from(PASS.to_owned());

    let bytes = std::fs::read(&config).unwrap();
    let decryptor = age::Decryptor::new_buffered(&bytes[..]).unwrap();
    let identity = age::scrypt::Identity::new(pass());
    let mut plain = Vec::new();
    decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .unwrap()
        .read_to_end(&mut plain)
        .unwrap();
    let mut recipient = age::scrypt::Recipient::new(pass());
    recipient.set_work_factor(19);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .unwrap();
    let mut raised = Vec::new();
    let mut writer = encryptor.wrap_output(&mut raised).unwrap();
    writer.write_all(&plain).unwrap();
    writer.finish().unwrap();
    std::fs::write(&config, raised).unwrap();
    assert_eq!(scrypt_log_n(&config), 19);

    let steps = vec![(PROMPT, format!("{PASS}\r").into_bytes())];
    let (ok, output) = dialog(
        &format!("prlimit --memlock=536870912:536870912 '{BIN}' diary"),
        &home,
        &steps,
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

/// Refused before any prompt, with a message containing `expected`.
fn assert_refused_before_prompt(home: &Path, expected: &str) {
    let (ok, output) = dialog(&format!("'{BIN}' diary"), home, &[]);
    assert!(!ok, "unlocked: {output}");
    assert!(!output.contains(PROMPT), "prompted: {output}");
    assert!(output.contains(expected), "want {expected:?}: {output}");
    assert_no_leak(home, &output);
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
        home.join("Vaults/other/identity.age"),
        vault.join("identity.age"),
    )
    .unwrap();
    assert_refused(&home, "vault.age: the file is not encrypted to this key");
}

/// identity.age sealed to another passphrase (changed elsewhere, or
/// planted): the config opens, the key does not.
#[test]
fn refuses_identity_under_another_passphrase() {
    let (home, vault) = home_with_vault("identity_other_pass");
    let elsewhere = private_home("identity_other_pass_elsewhere");
    init(&elsewhere, "diary", "another-passphrase-here");
    std::fs::copy(
        elsewhere.join("Vaults/diary/identity.age"),
        vault.join("identity.age"),
    )
    .unwrap();
    assert_refused(&home, "identity.age does not open with this passphrase");
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

/// VERSION is checked before any vault file is read: missing, a symlink,
/// a FIFO, malformed, or a format this build does not know.
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
            |f| std::fs::write(f, b"2\n").unwrap(),
            "a format this version of Crystal cannot read",
        ),
    ];
    for (test, damage, expected) in cases {
        let (home, vault) = home_with_vault(test);
        damage(&vault.join("VERSION"));
        assert_refused(&home, expected);
    }
}

#[test]
fn refuses_corrupt_vault_counter() {
    let (home, _) = home_with_vault("counter_corrupt");
    std::fs::write(vault_counter(&home), b"two\n").unwrap();
    assert_refused(
        &home,
        "the vault's generation file in ~/.local/state/crystal is malformed",
    );
}

#[test]
fn refuses_corrupt_config_counter() {
    let (home, _) = home_with_vault("config_counter_corrupt");
    std::fs::write(home.join(".local/state/crystal/config.gen"), b"\n").unwrap();
    assert_refused(
        &home,
        "the config's generation file in ~/.local/state/crystal is malformed",
    );
}

/// The vault's counter is gone (a new device, or a wiped state directory):
/// the unlock works, says once that rollback detection starts now, and
/// records the counter so the next unlock says nothing.
#[test]
fn first_unlock_on_a_device_says_so_once() {
    let (home, _) = home_with_vault("first_use");
    let counter = vault_counter(&home);
    std::fs::remove_file(&counter).unwrap();
    let first = "First unlock of this vault on this device";
    let output = unlock_ok(&home, "diary", PASS);
    assert!(output.contains(first), "{output}");
    assert_eq!(std::fs::read(&counter).unwrap(), b"1\n");
    let output = unlock_ok(&home, "diary", PASS);
    assert!(!output.contains(first), "{output}");
}

#[test]
fn refuses_config_that_is_not_a_regular_file() {
    let (home, _) = home_with_vault("config_symlink");
    make_symlink(&home.join(".config/crystal/config.age"));
    assert_refused_before_prompt(&home, "cannot read a Crystal file (not a regular file)");
    let (home, _) = home_with_vault("config_fifo");
    make_fifo(&home.join(".config/crystal/config.age"));
    assert_refused_before_prompt(&home, "cannot read a Crystal file (not a regular file)");
}

#[test]
fn refuses_oversized_config() {
    let (home, _) = home_with_vault("config_too_large");
    let config = home.join(".config/crystal/config.age");
    std::fs::write(&config, vec![b'x'; 33 << 20]).unwrap();
    assert_refused(&home, "config.age is too large");
}

// ---- crystal add, init, remove ----

fn config_file(home: &Path) -> PathBuf {
    home.join(".config/crystal/config.age")
}

/// The decrypted config, to look for names and paths in it.
fn config_plain(home: &Path) -> Vec<u8> {
    let scrypt = age::scrypt::Identity::new(PASS.to_owned().into());
    age_decrypt(&scrypt, &config_file(home))
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

/// The state directory's file names, sorted.
fn state_files(home: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(home.join(".local/state/crystal"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    files
}

/// `crystal add NAME DIR` with `steps`; returns success and the output.
fn add(home: &Path, name: &str, dir: &Path, steps: &[(&str, Vec<u8>)]) -> (bool, String) {
    dialog(
        &format!("'{BIN}' add {name} '{}'", dir.display()),
        home,
        steps,
    )
}

fn pass_step(pass: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![(PROMPT, format!("{pass}\r").into_bytes())]
}

/// A second device: the vault made on the first is added to a device with
/// no config. A new passphrase is set (it must be the vault's), the
/// counter starts at the vault's generation, and the vault unlocks.
#[test]
fn add_on_a_new_device() {
    let (_, vault) = home_with_vault("add_device_a");
    let home = private_home("add_device_b");
    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
    ];
    let (ok, output) = add(&home, "diary", &vault, &steps);
    assert!(ok, "{output}");
    assert!(!output.contains(PASS), "{output}");
    assert_eq!(state_files(&home).len(), 2, "config.gen and the vault's");
    assert_eq!(std::fs::read(vault_counter(&home)).unwrap(), b"1\n");
    let output = unlock_ok(&home, "diary", PASS);
    assert!(output.contains("0 notes unlocked"), "{output}");
}

/// A name already registered is refused once the config is open, before
/// the vault is read; nothing changes.
#[test]
fn add_refuses_a_taken_name() {
    let (home, _) = home_with_vault("add_taken");
    init(&home, "other", PASS);
    let other = home.join("Vaults/other");
    let config = std::fs::read(config_file(&home)).unwrap();
    let state = state_files(&home);
    let (ok, output) = add(&home, "diary", &other, &pass_step(PASS));
    assert!(!ok && output.contains("already registered"), "{output}");
    assert_no_leak(&home, &output);
    assert_eq!(std::fs::read(config_file(&home)).unwrap(), config);
    assert_eq!(state_files(&home), state);
}

#[test]
fn add_refuses_wrong_passphrase() {
    let (home, vault) = home_with_vault("add_wrong_pass");
    let (ok, output) = dialog(&format!("'{BIN}' remove diary"), &home, &pass_step(PASS));
    assert!(ok, "{output}");
    let config = std::fs::read(config_file(&home)).unwrap();
    let (ok, output) = add(&home, "diary", &vault, &pass_step("not-the-passphrase"));
    assert!(
        !ok && output.contains("config.age: wrong passphrase"),
        "{output}"
    );
    assert!(!output.contains("not-the-passphrase"), "{output}");
    assert_eq!(std::fs::read(config_file(&home)).unwrap(), config);
}

/// A directory without VERSION is not a vault: refused before any prompt.
#[test]
fn add_refuses_directory_without_version() {
    let home = private_home("add_no_version");
    let dir = home.join("empty");
    std::fs::create_dir(&dir).unwrap();
    set_mode(&dir, 0o700);
    let (ok, output) = add(&home, "diary", &dir, &[]);
    assert!(
        !ok && output.contains("the vault has no VERSION file"),
        "{output}"
    );
    assert!(!output.contains(PROMPT), "prompted: {output}");
    assert!(!config_file(&home).exists());
}

/// DIR given through a symlink (as /home is on Fedora Atomic) is resolved
/// first: the config stores the real path, never the link, and every
/// unlock walks the real path with O_NOFOLLOW.
#[test]
fn add_through_a_symlink_stores_the_real_path() {
    let (home, vault) = home_with_vault("add_symlink");
    let (ok, output) = dialog(&format!("'{BIN}' remove diary"), &home, &pass_step(PASS));
    assert!(ok, "{output}");
    let link = home.join("shortcut");
    std::os::unix::fs::symlink(&vault, &link).unwrap();
    let (ok, output) = add(&home, "diary", &link, &pass_step(PASS));
    assert!(ok, "{output}");
    let plain = config_plain(&home);
    assert!(contains(&plain, vault.to_str().unwrap()), "real path");
    assert!(!contains(&plain, "shortcut"), "link stored");
    unlock_ok(&home, "diary", PASS);
}

/// init on a device with a config asks for its passphrase once; a wrong
/// one creates nothing.
#[test]
fn init_refuses_wrong_config_passphrase() {
    let (home, _) = home_with_vault("init_wrong_pass");
    let config = std::fs::read(config_file(&home)).unwrap();
    let (ok, output) = dialog(
        &format!("'{BIN}' init other"),
        &home,
        &pass_step("not-the-passphrase"),
    );
    assert!(
        !ok && output.contains("config.age: wrong passphrase"),
        "{output}"
    );
    assert!(!home.join("Vaults/other").exists());
    assert_eq!(std::fs::read(config_file(&home)).unwrap(), config);
}

/// On a new device the passphrase is typed twice; a mismatch creates
/// nothing and shows neither entry.
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
    assert!(!home.join("Vaults").exists());
    assert!(!config_file(&home).exists());
}

/// The backup prompt insists on at least one key, explains a bad one and
/// asks again, and the key finally given recovers the vault.
#[test]
fn init_backup_recipient_prompts() {
    let home = private_home("init_recipients");
    let backup = age::x25519::Identity::generate();
    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
        (BACKUP_1, b"\r".to_vec()),
        (
            "At least one backup recipient is needed.",
            format!("{}\r", backup.to_string().expose_secret()).into_bytes(),
        ),
        (
            "That is a private key (AGE-...), not a public key",
            b"age1notakey\r".to_vec(),
        ),
        (
            "That is not an age X25519 public key",
            b"ssh-ed25519 AAAA\r".to_vec(),
        ),
        (
            "That is not an age X25519 public key",
            format!("{}\r", backup.to_public()).into_bytes(),
        ),
        (BACKUP_2, b"\r".to_vec()),
    ];
    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &steps);
    assert!(ok, "{output}");
    let vault = home.join("Vaults/diary");
    let key = age_decrypt(&backup, &vault.join("identity.backup.age"));
    assert!(key.starts_with(b"AGE-SECRET-KEY-1"));
    // The pasted backup secret is in no file Crystal wrote.
    let secret = backup.to_string();
    for file in std::fs::read_dir(&vault).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap();
        assert!(!contains(&bytes, secret.expose_secret()));
    }
    assert!(!contains(&config_plain(&home), secret.expose_secret()));
}

/// decision 0028: init and add on a device without a config refuse a new
/// passphrase under 20 characters, before the repeat, and create nothing.
#[test]
fn new_passphrase_needs_twenty_characters() {
    let short = vec![(NEW_PROMPT, b"nineteen-characters\r".to_vec())];
    let home = private_home("init_short_pass");
    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &short);
    assert!(!ok && output.contains("at least 20 characters"), "{output}");
    assert!(!output.contains(REPEAT), "{output}");
    assert!(!home.join("Vaults").exists());
    assert!(!config_file(&home).exists());

    let (_, vault) = home_with_vault("add_short_pass_a");
    let home = private_home("add_short_pass_b");
    let (ok, output) = add(&home, "diary", &vault, &short);
    assert!(!ok && output.contains("at least 20 characters"), "{output}");
    assert!(!config_file(&home).exists());
    assert!(state_files(&home).is_empty());
}

/// Up to 8 backup recipients, each able to recover the vault; after the
/// 8th, init stops asking.
#[test]
fn init_takes_at_most_eight_backup_recipients() {
    let home = private_home("init_eight");
    let backups: Vec<_> = (0..8).map(|_| age::x25519::Identity::generate()).collect();
    let mut steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
    ];
    for (i, backup) in backups.iter().enumerate() {
        let prompt: &'static str = [
            "Backup recipient 1",
            "Backup recipient 2",
            "Backup recipient 3",
            "Backup recipient 4",
            "Backup recipient 5",
            "Backup recipient 6",
            "Backup recipient 7",
            "Backup recipient 8",
        ][i];
        steps.push((prompt, format!("{}\r", backup.to_public()).into_bytes()));
    }
    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(!output.contains("Backup recipient 9"), "{output}");
    let file = home.join("Vaults/diary/identity.backup.age");
    let first = age_decrypt(&backups[0], &file);
    for backup in &backups[1..] {
        assert_eq!(age_decrypt(backup, &file), first);
    }
    unlock_ok(&home, "diary", PASS);
}

/// remove forgets one name; the other vaults still unlock, the removed
/// vault's files stay, and removing it again is refused.
#[test]
fn remove_keeps_the_other_vaults() {
    let (home, vault) = home_with_vault("remove_others");
    init(&home, "other", PASS);
    let (ok, output) = dialog(&format!("'{BIN}' remove diary"), &home, &pass_step(PASS));
    assert!(ok, "{output}");
    assert_eq!(std::fs::read_dir(&vault).unwrap().count(), 4);
    unlock_ok(&home, "other", PASS);

    let (ok, output) = dialog(&format!("'{BIN}' diary"), &home, &pass_step(PASS));
    assert!(
        !ok && output.contains("no vault with that name is registered"),
        "{output}"
    );
    let (ok, output) = dialog(&format!("'{BIN}' remove diary"), &home, &pass_step(PASS));
    assert!(
        !ok && output.contains("no vault with that name is registered"),
        "{output}"
    );
}

#[test]
fn remove_refuses_wrong_passphrase() {
    let (home, _) = home_with_vault("remove_wrong_pass");
    let config = std::fs::read(config_file(&home)).unwrap();
    let (ok, output) = dialog(
        &format!("'{BIN}' remove diary"),
        &home,
        &pass_step("not-the-passphrase"),
    );
    assert!(
        !ok && output.contains("config.age: wrong passphrase"),
        "{output}"
    );
    assert_eq!(std::fs::read(config_file(&home)).unwrap(), config);
    unlock_ok(&home, "diary", PASS);
}

/// U3: the same vault (same vault_id) cannot be registered under a second
/// name, even through another path to it; nothing is written.
#[test]
fn add_refuses_the_same_vault_under_another_name() {
    let (home, vault) = home_with_vault("add_same_vault");
    let link = home.join("again");
    std::os::unix::fs::symlink(&vault, &link).unwrap();
    let config = std::fs::read(config_file(&home)).unwrap();
    let state = state_files(&home);
    for dir in [&vault, &link] {
        let (ok, output) = add(&home, "alias", dir, &pass_step(PASS));
        assert!(
            !ok && output.contains("already registered on this device under another name"),
            "{output}"
        );
        assert_no_leak(&home, &output);
    }
    assert_eq!(std::fs::read(config_file(&home)).unwrap(), config);
    assert_eq!(state_files(&home), state);
}

/// U1: a device has one passphrase for its config and all its vaults. A
/// vault made under another passphrase is refused with a message that
/// says so, not as a bare "wrong passphrase"; nothing is written.
#[test]
fn add_explains_a_vault_under_another_passphrase() {
    let elsewhere = private_home("add_other_pass_elsewhere");
    init(&elsewhere, "work", "another-passphrase-here");
    let work = elsewhere.join("Vaults/work");

    let (home, _) = home_with_vault("add_other_pass");
    let config = std::fs::read(config_file(&home)).unwrap();
    let state = state_files(&home);
    let (ok, output) = add(&home, "work", &work, &pass_step(PASS));
    assert!(!ok, "{output}");
    assert!(
        output.contains("does not open with this device's passphrase"),
        "{output}"
    );
    assert!(
        output.contains("one passphrase for its config and every vault"),
        "{output}"
    );
    assert_eq!(std::fs::read(config_file(&home)).unwrap(), config);
    assert_eq!(state_files(&home), state);

    // On a device without a config, the passphrase typed was new: the
    // message asks for the vault's own.
    let fresh = private_home("add_other_pass_new_device");
    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
    ];
    let (ok, output) = add(&fresh, "work", &work, &steps);
    assert!(!ok, "{output}");
    assert!(
        output.contains("enter the passphrase the vault was made with"),
        "{output}"
    );
    assert!(!config_file(&fresh).exists());
}
