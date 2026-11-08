//! Acceptance through the real binary: unlock (startup step 8), the tamper
//! suite, and the vault commands (`init`, `recover`, `check-code`, `rm`).
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
/// output, which shows the recovery code (`recovery_code_in`).
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

/// Unlocks `name` with `pass` and quits at the placeholder screen.
fn unlock_ok(home: &Path, name: &str, pass: &str) -> String {
    let mut steps = pass_step(pass);
    steps.push(("notes unlocked", b"q".to_vec()));
    let (ok, output) = dialog(&format!("'{BIN}' {name}"), home, &steps);
    assert!(ok, "{output}");
    output
}

/// Tries to unlock `diary` with `PASS`; it must be refused with a message
/// containing `expected`, and never show HOME's path or the passphrase.
fn assert_refused(home: &Path, expected: &str) {
    let (ok, output) = dialog(&format!("'{BIN}' diary"), home, &pass_step(PASS));
    assert!(!ok, "unlocked: {output}");
    assert!(output.contains(expected), "want {expected:?}: {output}");
    assert!(!output.contains("notes unlocked"), "{output}");
    assert_no_leak(home, &output);
}

/// Refused before any prompt, with a message containing `expected`.
fn assert_refused_before_prompt(home: &Path, expected: &str) {
    let (ok, output) = dialog(&format!("'{BIN}' diary"), home, &[]);
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

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
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
        &format!("'{BIN}' diary"),
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

/// An older vault.age restored after this device saw a newer one: the
/// device's counter is ahead of the file.
#[test]
fn refuses_rolled_back_vault() {
    let (home, _) = home_with_vault("vault_rollback");
    std::fs::write(vault_counter(&home, "diary"), b"2\n").unwrap();
    assert_refused(
        &home,
        "the vault is older than the last one this device has seen (generation 1, last seen 2)",
    );
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

/// Phase 2 refuses a vault with a Syncthing conflict copy; Phase 4 merges.
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
    let (ok, output) = dialog(&format!("'{BIN}' work"), &home, &[]);
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

/// docs/decisions/0027-scrypt-fixed.md: every passphrase file is written
/// at log N 18, whatever this device's memlock limit or speed, so every
/// device at the floor opens it.
#[test]
fn passphrase_files_use_log_n_18() {
    let (_, vault) = home_with_vault("scrypt_factor");
    assert_eq!(scrypt_log_n(&vault.join("identity.age")), 18);
    assert_eq!(scrypt_log_n(&vault.join("identity.backup.age")), 18);
}

/// An identity.age asking for log N 19 (512 MiB of scrypt) on a device at
/// the 512 MiB floor, where only 256 MiB may go to scrypt: refused before
/// any scrypt work with a message, never an abort.
#[test]
fn refuses_work_factor_beyond_locked_memory() {
    use age::secrecy::SecretString;
    use std::io::Write;
    let (home, vault) = home_with_vault("scrypt_too_high");
    let identity = vault.join("identity.age");
    let pass = || SecretString::from(PASS.to_owned());

    let plain = age_decrypt(&age::scrypt::Identity::new(pass()), &identity);
    let mut recipient = age::scrypt::Recipient::new(pass());
    recipient.set_work_factor(19);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .unwrap();
    let mut raised = Vec::new();
    let mut writer = encryptor.wrap_output(&mut raised).unwrap();
    writer.write_all(&plain).unwrap();
    writer.finish().unwrap();
    std::fs::write(&identity, raised).unwrap();
    assert_eq!(scrypt_log_n(&identity), 19);

    let (ok, output) = dialog(
        &format!("prlimit --memlock=536870912:536870912 '{BIN}' diary"),
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
            |f| std::fs::write(f, b"2\n").unwrap(),
            "a format this version of Crystal cannot read",
        ),
    ];
    for (test, damage, expected) in cases {
        let (home, vault) = home_with_vault(test);
        damage(&vault.join("VERSION"));
        assert_refused_before_prompt(&home, expected);
    }
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

/// The recovery code is shown once and must be typed back. A wrong or
/// mistyped entry shows the code again. The code then opens
/// identity.backup.age, and appears in no file.
#[test]
fn init_recovery_code_confirmation() {
    let home = private_home("init_recovery_confirm");
    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
        (RECOVERY_SHOWN, b"\r".to_vec()),
        (
            RECOVERY_CONFIRM,
            b"0000-0000-0000-0000-0000-0000-0000\r".to_vec(),
        ),
        ("That is not the code shown; here it is again.", Vec::new()),
        (RECOVERY_SHOWN, b"\r".to_vec()),
        (RECOVERY_CONFIRM, b"K7QM-3XRD\r".to_vec()),
        ("That has the wrong length", Vec::new()),
        (RECOVERY_SHOWN, b"\r".to_vec()),
        (RECOVERY_CONFIRM, TYPE_RECOVERY_CODE.to_vec()),
    ];
    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &steps);
    assert!(ok, "{output}");
    assert!(
        output.contains(
            "Crystal recommends a passphrase of 8 or more random words, generated with \
             KeePassDX or KeePassXC."
        ),
        "{output}"
    );
    let code = recovery_code_in(&output);
    let canonical = code.replace('-', "");
    let vault = vault_path(&home, "diary");
    let by_code = age::scrypt::Identity::new(canonical.clone().into());
    let key = age_decrypt(&by_code, &vault.join("identity.backup.age"));
    assert!(key.starts_with(b"AGE-SECRET-KEY-1"));
    for dir in [vault.clone(), state_dir(&home)] {
        for file in std::fs::read_dir(&dir).unwrap() {
            let bytes = std::fs::read(file.unwrap().path()).unwrap();
            assert!(!contains(&bytes, &canonical) && !contains(&bytes, &code));
        }
    }
    unlock_ok(&home, "diary", PASS);
}

/// A typo in the code typed back (one symbol changed) is caught by the
/// check symbol, and the code is shown again.
#[test]
fn init_recovery_code_typo_is_caught() {
    let home = private_home("init_recovery_typo");
    let steps = vec![
        (NEW_PROMPT, format!("{PASS}\r").into_bytes()),
        (REPEAT, format!("{PASS}\r").into_bytes()),
        (RECOVERY_SHOWN, b"\r".to_vec()),
        (RECOVERY_CONFIRM, TYPE_RECOVERY_CODE_WITH_TYPO.to_vec()),
        ("That has a typo; here is the code again.", Vec::new()),
        (RECOVERY_SHOWN, b"\r".to_vec()),
        (RECOVERY_CONFIRM, TYPE_RECOVERY_CODE_LOWERCASE.to_vec()),
    ];
    let (ok, output) = dialog(&format!("'{BIN}' init diary"), &home, &steps);
    assert!(ok, "{output}");
}

/// decision 0028: init and recover refuse a new passphrase under 20
/// characters, before the repeat, and change nothing.
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

    let home = private_home("recover_short_pass");
    let code = recovery_code_in(&init(&home, "diary", PASS));
    let identity = std::fs::read(vault_path(&home, "diary").join("identity.age")).unwrap();
    let (ok, output) = recover(&home, &[code_step(&code), short]);
    assert!(!ok && output.contains("at least 20 characters"), "{output}");
    assert_eq!(
        std::fs::read(vault_path(&home, "diary").join("identity.age")).unwrap(),
        identity
    );
}

/// Each vault has its own passphrase.
#[test]
fn vaults_have_their_own_passphrases() {
    let (home, _) = home_with_vault("own_passphrases");
    let other = "a-second-vault-passphrase";
    init(&home, "work", other);
    unlock_ok(&home, "work", other);
    unlock_ok(&home, "diary", PASS);
    let (ok, output) = dialog(&format!("'{BIN}' work"), &home, &pass_step(PASS));
    assert!(
        !ok && output.contains("does not open with this passphrase"),
        "{output}"
    );
}

// ---- crystal recover and check-code ----

const NEW_PASS: &str = "a-new-passphrase-after-recovery";

/// `crystal recover diary` with `steps`.
fn recover(home: &Path, steps: &[(&str, Vec<u8>)]) -> (bool, String) {
    dialog(&format!("'{BIN}' recover diary"), home, steps)
}

fn code_step(code: &str) -> (&'static str, Vec<u8>) {
    (CODE_PROMPT, format!("{code}\r").into_bytes())
}

fn new_pass_steps() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (NEW_PROMPT, format!("{NEW_PASS}\r").into_bytes()),
        (REPEAT, format!("{NEW_PASS}\r").into_bytes()),
    ]
}

/// The main story: the passphrase is forgotten. The recovery code written
/// down at init (typed in lowercase here) opens the vault; the new
/// passphrase unlocks it and the old one no longer does. Only
/// identity.age changes.
#[test]
fn recover_after_forgetting_the_passphrase() {
    let home = private_home("recover_forgotten");
    let code = recovery_code_in(&init(&home, "diary", PASS));
    let vault = vault_path(&home, "diary");
    let before = std::fs::read(vault.join("vault.age")).unwrap();
    let mut steps = vec![code_step(&code.to_lowercase())];
    steps.extend(new_pass_steps());
    let (ok, output) = recover(&home, &steps);
    assert!(ok, "{output}");
    assert!(!output.contains(NEW_PASS), "{output}");
    unlock_ok(&home, "diary", NEW_PASS);
    let (ok, output) = dialog(&format!("'{BIN}' diary"), &home, &pass_step(PASS));
    assert!(
        !ok && output.contains("does not open with this passphrase"),
        "{output}"
    );
    assert_eq!(std::fs::read(vault.join("vault.age")).unwrap(), before);
}

/// identity.age lost: recover writes it again with a new passphrase.
#[test]
fn recover_a_lost_identity() {
    let home = private_home("recover_lost_identity");
    let code = recovery_code_in(&init(&home, "diary", PASS));
    std::fs::remove_file(vault_path(&home, "diary").join("identity.age")).unwrap();
    let mut steps = vec![code_step(&code)];
    steps.extend(new_pass_steps());
    let (ok, output) = recover(&home, &steps);
    assert!(ok, "{output}");
    unlock_ok(&home, "diary", NEW_PASS);
}

/// A typo, a short code, and another vault's code are refused with their
/// own messages, before any passphrase is asked for; nothing changes.
#[test]
fn recover_refuses_wrong_codes() {
    let home = private_home("recover_wrong_codes");
    let code = recovery_code_in(&init(&home, "diary", PASS));
    let other_code = recovery_code_in(&init(&home, "other", PASS));
    let vault = vault_path(&home, "diary");
    let identity = std::fs::read(vault.join("identity.age")).unwrap();
    let typo = format!(
        "{}{}",
        if code.starts_with('A') { "B" } else { "A" },
        &code[1..]
    );
    for (typed, expected) in [
        (typo.as_str(), "that recovery code has a typo"),
        ("K7QM-3XRD", "a recovery code has 28 characters"),
        (
            other_code.as_str(),
            "this recovery code does not open the vault's identity.backup.age",
        ),
    ] {
        for command in ["recover", "check-code"] {
            let (ok, output) = dialog(
                &format!("'{BIN}' {command} diary"),
                &home,
                &[code_step(typed)],
            );
            assert!(
                !ok && output.contains(expected),
                "{command} {typed}: {output}"
            );
            assert!(
                !output.contains("assphrase"),
                "asked for a passphrase: {output}"
            );
            assert_no_leak(&home, &output);
        }
    }
    assert_eq!(std::fs::read(vault.join("identity.age")).unwrap(), identity);
}

/// check-code confirms a right code and passphrase, and finds a wrong
/// passphrase and a backup that holds another key. It writes nothing.
#[test]
fn check_code_reports_and_writes_nothing() {
    let home = private_home("check_code");
    let code = recovery_code_in(&init(&home, "diary", PASS));
    init(&home, "other", PASS);
    let vault = vault_path(&home, "diary");
    let state = state_files(&home);
    let check = |pass: &str| {
        dialog(
            &format!("'{BIN}' check-code diary"),
            &home,
            &[code_step(&code), (PROMPT, format!("{pass}\r").into_bytes())],
        )
    };

    let (ok, output) = check(PASS);
    assert!(
        ok && output.contains("The recovery code is right"),
        "{output}"
    );
    let (ok, output) = check("not-the-passphrase");
    assert!(
        !ok && output.contains("identity.age does not open with this passphrase"),
        "{output}"
    );

    std::fs::copy(
        vault_path(&home, "other").join("identity.age"),
        vault.join("identity.age"),
    )
    .unwrap();
    let identity = std::fs::read(vault.join("identity.age")).unwrap();
    let (ok, output) = check(PASS);
    assert!(
        !ok && output.contains("identity.backup.age and identity.age hold different keys"),
        "{output}"
    );
    assert_eq!(std::fs::read(vault.join("identity.age")).unwrap(), identity);
    assert_eq!(state_files(&home), state);
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
    let (ok, output) = dialog(&format!("'{BIN}' diary"), &home, &[]);
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
    assert_eq!(std::fs::read_dir(&vault).unwrap().count(), 4);
    unlock_ok(&home, "diary", PASS);
}
