//! Phase 2 acceptance through the real binary (docs/development/roadmap.md):
//! unlock (startup step 8) and the tamper suite. Each test makes its own
//! vault with `crystal init` in a fresh HOME, damages one thing, and checks that
//! unlock refuses it with a fixed message that names no path.
//!
//! Like tests/startup.rs, these need RLIMIT_MEMLOCK at the floor.

mod common;

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
    init(&elsewhere, "diary", "another-passphrase");
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
