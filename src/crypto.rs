//! Crypto boundary: the age identity and encrypt/decrypt of the vault blob.
//! Plaintext above this module, ciphertext below.
//!
//! Everything goes through the `age` crate (rage's library). This module
//! adds four things on top: size limits checked before any allocation,
//! zeroizing buffers for every plaintext it returns, error values that
//! carry no text from age or from the input, and types that keep secrets
//! from reaching Debug, Display, Clone or serde.

use std::fmt;
use std::io::{Read, Write};
use std::iter;

use age::{DecryptError, Decryptor, Encryptor, scrypt, x25519};
use rustix::io::Errno;
use rustix::rand::{GetRandomFlags, getrandom};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::model::assert_not_impl;

/// Fills `buf` from the kernel's cryptographically secure generator (the
/// getrandom syscall, through rustix). No flags: it blocks until the pool
/// is seeded at boot instead of returning weak bytes, and never reads
/// /dev/urandom, so it needs no file access inside the sandbox. Retries on
/// EINTR and on short reads.
pub fn fill_random(buf: &mut [u8]) -> Result<(), Errno> {
    let mut filled = 0;
    while filled < buf.len() {
        match getrandom(&mut buf[filled..], GetRandomFlags::empty()) {
            Ok(n) => filled += n,
            Err(Errno::INTR) => {}
            Err(errno) => return Err(errno),
        }
    }
    Ok(())
}

/// The largest plaintext Crystal encrypts: 32 MiB, far beyond any text
/// vault, and well inside the 512 MiB locked-memory minimum
/// (docs/design/startup.md, step 4) with the ciphertext and a decoded
/// copy alongside.
pub const MAX_PLAINTEXT: usize = 32 << 20;

/// The largest ciphertext Crystal decrypts: `MAX_PLAINTEXT`, plus age's 16
/// bytes per 64 KiB chunk, plus 64 KiB for the header (stanzas, MAC,
/// nonce). Checked before anything is parsed or allocated.
pub const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + MAX_PLAINTEXT / 4096 + (64 << 10);

/// age's scrypt work factor (log2 N) in test builds only, so the tests run
/// in milliseconds. Every other build uses `scrypt_policy`, and the release
/// binary has no way to lower it.
#[cfg(test)]
const TEST_SCRYPT_LOG_N: u8 = 10;

/// A ceiling far above any timing target (2^30 needs 1 TiB), for an
/// unlimited RLIMIT_MEMLOCK.
const MAX_SCRYPT_LOG_N: u8 = 30;

/// scrypt work factors for this run (docs/decisions/0025-scrypt-budget.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScryptPolicy {
    /// log2 N for new passphrase files.
    encrypt: u8,
    /// The largest log2 N accepted when decrypting; above it, a file is
    /// refused as WorkFactorTooHigh before any scrypt work.
    max_decrypt: u8,
}

/// The largest log2 N whose scrypt memory (128 * r * N bytes, r = 8 in
/// age, so 2^(log N + 10)) fits in half of the locked-memory limit
/// `memlock` (`None`: unlimited). Every allocation is locked (mlockall with
/// MCL_FUTURE), so scrypt beyond the limit would abort; the other half
/// stays free for the vault and everything else.
fn fitting_log_n(memlock: Option<u64>) -> u8 {
    let Some(limit) = memlock else {
        return MAX_SCRYPT_LOG_N;
    };
    let half = limit / 2;
    if half < 1 << 11 {
        return 1;
    }
    let log2 = 63 - half.leading_zeros();
    u8::try_from(log2 - 10)
        .unwrap_or(MAX_SCRYPT_LOG_N)
        .min(MAX_SCRYPT_LOG_N)
}

/// The work factor that takes about one second here, measured the way age
/// measures it (age 0.11.5, `target_scrypt_work_factor`): time log N = 10,
/// raise it until the time is measurable, then double until one second.
/// age does not expose its own choice, so Crystal repeats the measurement.
#[cfg(not(test))]
fn timed_log_n() -> u8 {
    use std::time::{Duration, Instant};
    let measure = |log_n: u8| {
        let mut recipient = scrypt::Recipient::new(SecretString::from(String::new()));
        recipient.set_work_factor(log_n);
        let start = Instant::now();
        let _ = encrypt(iter::once(&recipient as &dyn age::Recipient), b"");
        start.elapsed()
    };
    let mut log_n = 10;
    let mut elapsed = measure(log_n);
    while elapsed.is_zero() && log_n < MAX_SCRYPT_LOG_N {
        log_n += 1;
        elapsed = measure(log_n);
    }
    while elapsed < Duration::from_secs(1) && log_n < MAX_SCRYPT_LOG_N {
        log_n += 1;
        elapsed *= 2;
    }
    log_n
}

/// age's own policy (about one second to encrypt; files up to 16 times
/// that accepted), each capped by what fits in locked memory. Computed once
/// per run.
fn scrypt_policy() -> ScryptPolicy {
    #[cfg(test)]
    {
        ScryptPolicy {
            encrypt: TEST_SCRYPT_LOG_N,
            max_decrypt: TEST_SCRYPT_LOG_N + 4,
        }
    }
    #[cfg(not(test))]
    {
        static POLICY: std::sync::OnceLock<ScryptPolicy> = std::sync::OnceLock::new();
        *POLICY.get_or_init(|| {
            let memlock = rustix::process::getrlimit(rustix::process::Resource::Memlock).maximum;
            let fit = fitting_log_n(memlock);
            let target = timed_log_n();
            ScryptPolicy {
                encrypt: target.min(fit),
                max_decrypt: target.saturating_add(4).min(fit),
            }
        })
    }
}

/// Why encryption or decryption failed. Fixed variants only: age's error
/// text, file contents and passphrases never reach a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The scrypt stanza did not open with this passphrase. A bit flip in
    /// that stanza looks exactly the same: age cannot tell them apart.
    WrongPassphrase,
    /// The file is not encrypted to this key: no stanza matches, or a
    /// passphrase was given for a key-encrypted file, or the other way round.
    NotForThisKey,
    /// Not a valid age file: bad or truncated header, wrong MAC, damaged
    /// or truncated payload, unknown version, or a scrypt stanza mixed
    /// with other stanzas.
    Corrupt,
    /// Over `MAX_PLAINTEXT` or `MAX_CIPHERTEXT`.
    TooLarge,
    /// The file asks for a scrypt work factor far above what this machine
    /// manages in a second (age refuses over 16 times its own target), so
    /// a planted file cannot make unlock run for hours.
    WorkFactorTooHigh,
    /// `encrypt_to_recipients` was given no recipients.
    NoRecipients,
    /// age refused to encrypt (cannot happen with the recipient types used
    /// here; kept so no age error is ever passed through).
    EncryptFailed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WrongPassphrase => "wrong passphrase",
            Self::NotForThisKey => "the file is not encrypted to this key",
            Self::Corrupt => "the encrypted file is damaged or not an age file",
            Self::TooLarge => "the encrypted file is too large",
            Self::WorkFactorTooHigh => {
                "the passphrase file needs more scrypt work or memory than Crystal allows here \
                 (a higher RLIMIT_MEMLOCK allows more memory; see docs/guide/setup.md)"
            }
            Self::NoRecipients => "no recipients to encrypt to",
            Self::EncryptFailed => "encryption failed",
        })
    }
}

impl std::error::Error for CryptoError {}

/// Bytes produced by age encryption. The field is private and the only
/// constructors live in this module, so code outside `crypto` cannot label
/// arbitrary bytes as ciphertext and hand them to `vault::atomic_write`.
pub struct Ciphertext(Vec<u8>);

impl Ciphertext {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Tests in other modules need ciphertext without encrypting. Test
    /// builds only: the real program has no way in.
    #[cfg(test)]
    pub fn for_test(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// A passphrase: non-empty UTF-8 (age's scrypt takes a `&str`), in a
/// buffer wiped on drop. Deliberately no Debug, Display, Clone or
/// Serialize.
pub struct Passphrase(Zeroizing<Vec<u8>>);

assert_not_impl!(Passphrase: fmt::Debug, fmt::Display, serde::Serialize, Clone);

impl Passphrase {
    /// None for an empty or non-UTF-8 passphrase.
    pub fn new(bytes: Zeroizing<Vec<u8>>) -> Option<Self> {
        let valid = !bytes.is_empty() && std::str::from_utf8(&bytes).is_ok();
        valid.then_some(Self(bytes))
    }

    /// A copy for age, which wipes it on drop. Built with the exact
    /// capacity, so turning it into a boxed `str` never reallocates and
    /// leaves no unwiped buffer behind.
    fn to_secret_string(&self) -> SecretString {
        // `new` checked UTF-8; the fallback can't happen and holds no data.
        let text = std::str::from_utf8(&self.0).unwrap_or_default();
        let mut copy = String::with_capacity(text.len());
        copy.push_str(text);
        SecretString::from(copy)
    }

    /// Whether both passphrases are the same, for `crystal init`'s "type it
    /// again". The time taken depends only on the lengths, never on where
    /// the first difference is: every byte pair is XORed and OR-folded, and
    /// black_box keeps the compiler from adding an early exit.
    pub fn matches(&self, other: &Passphrase) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        let diff = self
            .0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | std::hint::black_box(a ^ b));
        std::hint::black_box(diff) == 0
    }

    /// Debug and test builds only: the length in bytes, for the prompt
    /// tests (the `len=<n>` line and the unit tests). Never the content.
    #[cfg(any(test, debug_assertions))]
    pub fn len_for_test(&self) -> usize {
        self.0.len()
    }
}

/// The vault's X25519 identity. Only ever on the heap; age's key type
/// (x25519-dalek's `StaticSecret`, `zeroize` feature on) wipes itself when
/// dropped. It is never serialized except into an age encryption call
/// here, and has no Debug, Display, Clone or Serialize.
///
/// Not a `secrecy::SecretBox` as #8 first proposed: `SecretBox` needs the
/// inner type to implement `Zeroize`, and age's identity does not; it
/// wipes on drop instead, which is what `SecretBox` would add.
pub struct VaultIdentity(Box<x25519::Identity>);

assert_not_impl!(VaultIdentity: fmt::Debug, fmt::Display, serde::Serialize, Clone);

impl VaultIdentity {
    /// A new random identity, from the kernel's generator (through age).
    pub fn generate() -> Self {
        Self(Box::new(x25519::Identity::generate()))
    }

    /// The public key `vault.age` is encrypted to.
    pub fn recipient(&self) -> x25519::Recipient {
        self.0.to_public()
    }

    /// `identity.age` for passphrase unlock: the identity's age string
    /// (`AGE-SECRET-KEY-1…`), encrypted to the passphrase alone.
    pub fn seal_with_passphrase(&self, pass: &Passphrase) -> Result<Ciphertext, CryptoError> {
        let secret = self.0.to_string();
        encrypt_to_passphrase(secret.expose_secret().as_bytes(), pass)
    }

    /// `identity.backup.age`: the same string, encrypted to the offline
    /// backup recipient(s).
    pub fn seal_to_recipients(
        &self,
        recipients: &[x25519::Recipient],
    ) -> Result<Ciphertext, CryptoError> {
        let secret = self.0.to_string();
        encrypt_to_recipients(secret.expose_secret().as_bytes(), recipients)
    }

    /// Unlock: decrypts `identity.age` and parses the identity. Content
    /// that decrypts but is not an identity is `Corrupt`.
    pub fn open_with_passphrase(ciphertext: &[u8], pass: &Passphrase) -> Result<Self, CryptoError> {
        let secret = decrypt_with_passphrase(ciphertext, pass)?;
        let text = std::str::from_utf8(&secret).map_err(|_| CryptoError::Corrupt)?;
        let identity = text.parse().map_err(|_| CryptoError::Corrupt)?;
        Ok(Self(Box::new(identity)))
    }
}

/// Encrypts `secret` to a passphrase: one scrypt stanza as the only
/// recipient, as age requires.
pub fn encrypt_to_passphrase(secret: &[u8], pass: &Passphrase) -> Result<Ciphertext, CryptoError> {
    let mut recipient = scrypt::Recipient::new(pass.to_secret_string());
    recipient.set_work_factor(scrypt_policy().encrypt);
    encrypt(iter::once(&recipient as &dyn age::Recipient), secret)
}

/// Encrypts `secret` to X25519 recipients: the vault recipient for
/// `vault.age`, the backup recipients for `identity.backup.age`.
pub fn encrypt_to_recipients(
    secret: &[u8],
    recipients: &[x25519::Recipient],
) -> Result<Ciphertext, CryptoError> {
    if recipients.is_empty() {
        return Err(CryptoError::NoRecipients);
    }
    encrypt(recipients.iter().map(|r| r as &dyn age::Recipient), secret)
}

fn encrypt<'a>(
    recipients: impl Iterator<Item = &'a dyn age::Recipient>,
    secret: &[u8],
) -> Result<Ciphertext, CryptoError> {
    if secret.len() > MAX_PLAINTEXT {
        return Err(CryptoError::TooLarge);
    }
    let encryptor =
        Encryptor::with_recipients(recipients).map_err(|_| CryptoError::EncryptFailed)?;
    // Ciphertext is not secret; the capacity is only a hint.
    let mut out = Vec::with_capacity(secret.len() + secret.len() / 4096 + 1024);
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|_| CryptoError::EncryptFailed)?;
    writer
        .write_all(secret)
        .map_err(|_| CryptoError::EncryptFailed)?;
    writer.finish().map_err(|_| CryptoError::EncryptFailed)?;
    Ok(Ciphertext(out))
}

/// Decrypts a passphrase-encrypted file. A file with any other stanza is
/// `NotForThisKey` (or `Corrupt` if it mixes scrypt with other stanzas,
/// which age forbids).
pub fn decrypt_with_passphrase(
    ciphertext: &[u8],
    pass: &Passphrase,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let mut identity = scrypt::Identity::new(pass.to_secret_string());
    // Refuse, before any scrypt work, a file asking for more than this run
    // allows: age's own limit could exceed locked memory and abort.
    identity.set_max_work_factor(scrypt_policy().max_decrypt);
    decrypt(ciphertext, &identity, Expect::Passphrase)
}

/// Decrypts a file encrypted to `identity` (`vault.age`).
pub fn decrypt_with_identity(
    ciphertext: &[u8],
    identity: &VaultIdentity,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    decrypt(ciphertext, &*identity.0, Expect::Key)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Passphrase,
    Key,
}

fn decrypt(
    ciphertext: &[u8],
    identity: &dyn age::Identity,
    expect: Expect,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if ciphertext.len() > MAX_CIPHERTEXT {
        return Err(CryptoError::TooLarge);
    }
    let decryptor = Decryptor::new_buffered(ciphertext).map_err(|e| map_decrypt(e, expect))?;
    if decryptor.is_scrypt() != (expect == Expect::Passphrase) {
        return Err(CryptoError::NotForThisKey);
    }
    let mut reader = decryptor
        .decrypt(iter::once(identity))
        .map_err(|e| map_decrypt(e, expect))?;
    // The plaintext is shorter than the ciphertext (header and tags), so
    // this buffer never grows and no unwiped copy is left behind.
    let mut plaintext = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    reader
        .read_to_end(&mut plaintext)
        .map_err(|_| CryptoError::Corrupt)?;
    Ok(plaintext)
}

/// age's error to a fixed variant; nothing of age's message is kept.
fn map_decrypt(e: DecryptError, expect: Expect) -> CryptoError {
    match e {
        DecryptError::DecryptionFailed | DecryptError::KeyDecryptionFailed => match expect {
            Expect::Passphrase => CryptoError::WrongPassphrase,
            Expect::Key => CryptoError::NotForThisKey,
        },
        DecryptError::NoMatchingKeys => CryptoError::NotForThisKey,
        DecryptError::ExcessiveWork { .. } => CryptoError::WorkFactorTooHigh,
        _ => CryptoError::Corrupt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn fill_random_fills_and_differs() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        fill_random(&mut a).unwrap();
        fill_random(&mut b).unwrap();
        // 2^-256 chance of a false failure for each assertion.
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b);
        fill_random(&mut []).unwrap();
    }

    fn pass(s: &str) -> Passphrase {
        Passphrase::new(Zeroizing::new(s.as_bytes().to_vec())).unwrap()
    }

    fn sealed_to(identity: &VaultIdentity, secret: &[u8]) -> Vec<u8> {
        encrypt_to_recipients(secret, &[identity.recipient()])
            .unwrap()
            .0
    }

    fn sealed_with(p: &str, secret: &[u8]) -> Vec<u8> {
        encrypt_to_passphrase(secret, &pass(p)).unwrap().0
    }

    #[test]
    fn passphrase_must_be_non_empty_utf8() {
        let new = |b: &[u8]| Passphrase::new(Zeroizing::new(b.to_vec())).is_some();
        assert!(new(b"correct horse"));
        assert!(new("pässwörd".as_bytes()));
        assert!(!new(b""));
        assert!(!new(b"\xff\xfe"));
    }

    // scrypt memory is 2^(log N + 10) bytes; half the memlock limit may go
    // to it.
    #[test]
    fn scrypt_fits_half_of_locked_memory() {
        const MIB: u64 = 1 << 20;
        assert_eq!(fitting_log_n(Some(256 * MIB)), 17);
        assert_eq!(fitting_log_n(Some(300 * MIB)), 17);
        assert_eq!(fitting_log_n(Some(512 * MIB)), 18, "the floor: 256 MiB");
        assert_eq!(fitting_log_n(Some(4096 * MIB)), 21);
        assert_eq!(fitting_log_n(None), MAX_SCRYPT_LOG_N);
        assert_eq!(fitting_log_n(Some(u64::MAX)), MAX_SCRYPT_LOG_N);
        assert_eq!(fitting_log_n(Some(1024)), 1);
        for log_n in 2..=MAX_SCRYPT_LOG_N {
            let limit = 2u64 << (u32::from(log_n) + 10);
            assert_eq!(fitting_log_n(Some(limit)), log_n, "{log_n}");
            assert_eq!(fitting_log_n(Some(limit - 1)), log_n - 1, "{log_n}");
        }
    }

    #[test]
    fn passphrase_matches() {
        assert!(pass("abc").matches(&pass("abc")));
        assert!(!pass("abc").matches(&pass("abd")));
        assert!(!pass("abc").matches(&pass("abcd")));
        assert!(!pass("xbc").matches(&pass("abc")));
    }

    #[test]
    fn passphrase_round_trip_and_wrong_passphrase() {
        let file = sealed_with("correct horse", b"secret");
        let plain = decrypt_with_passphrase(&file, &pass("correct horse")).unwrap();
        assert_eq!(&plain[..], b"secret");
        let wrong = decrypt_with_passphrase(&file, &pass("wrong horse"));
        assert_eq!(wrong.unwrap_err(), CryptoError::WrongPassphrase);
    }

    #[test]
    fn recipients_round_trip_and_other_keys_refused() {
        let (a, b, other) = (
            VaultIdentity::generate(),
            VaultIdentity::generate(),
            VaultIdentity::generate(),
        );
        let file = encrypt_to_recipients(b"vault", &[a.recipient(), b.recipient()]).unwrap();
        for id in [&a, &b] {
            assert_eq!(&decrypt_with_identity(&file.0, id).unwrap()[..], b"vault");
        }
        let err = decrypt_with_identity(&file.0, &other).unwrap_err();
        assert_eq!(err, CryptoError::NotForThisKey);
        let none = encrypt_to_recipients(b"vault", &[]).err();
        assert_eq!(none, Some(CryptoError::NoRecipients));
    }

    // A passphrase file is never opened with a key, nor the other way round.
    #[test]
    fn passphrase_and_key_files_are_not_interchangeable() {
        let id = VaultIdentity::generate();
        let by_pass = sealed_with("pw", b"x");
        let by_key = sealed_to(&id, b"x");
        let err = decrypt_with_identity(&by_pass, &id).unwrap_err();
        assert_eq!(err, CryptoError::NotForThisKey);
        let err = decrypt_with_passphrase(&by_key, &pass("pw")).unwrap_err();
        assert_eq!(err, CryptoError::NotForThisKey);
    }

    #[test]
    fn vault_identity_seal_and_open() {
        let id = VaultIdentity::generate();
        let file = id.seal_with_passphrase(&pass("pw")).unwrap();
        let opened = VaultIdentity::open_with_passphrase(&file.0, &pass("pw")).unwrap();
        assert_eq!(opened.recipient().to_string(), id.recipient().to_string());
        let vault = sealed_to(&id, b"notes");
        assert_eq!(
            &decrypt_with_identity(&vault, &opened).unwrap()[..],
            b"notes"
        );

        // identity.backup.age opens with the backup key.
        let backup = VaultIdentity::generate();
        let file = id.seal_to_recipients(&[backup.recipient()]).unwrap();
        let text = decrypt_with_identity(&file.0, &backup).unwrap();
        assert!(text.starts_with(b"AGE-SECRET-KEY-1"));

        // Decrypts, but is not an identity.
        let junk = sealed_with("pw", b"not a key");
        let err = VaultIdentity::open_with_passphrase(&junk, &pass("pw")).err();
        assert_eq!(err, Some(CryptoError::Corrupt));
    }

    // Every single-bit flip at every byte of a small key-encrypted file,
    // and every truncation, is refused.
    #[test]
    fn key_file_bit_flips_and_truncation_are_refused() {
        let id = VaultIdentity::generate();
        let file = sealed_to(&id, b"a short vault");
        for i in 0..file.len() {
            let mut bad = file.clone();
            bad[i] ^= 1 << (i % 8);
            assert!(decrypt_with_identity(&bad, &id).is_err(), "flip at {i}");
        }
        for len in 0..file.len() {
            assert!(
                decrypt_with_identity(&file[..len], &id).is_err(),
                "len {len}"
            );
        }
    }

    // A two-chunk payload (age chunks are 64 KiB plus a 16-byte tag): flips
    // in either chunk and cutting the file at the chunk boundary (a valid
    // first chunk alone) are refused.
    #[test]
    fn multi_chunk_flips_and_truncation_are_refused() {
        const CHUNK: usize = 64 << 10;
        const TAG: usize = 16;
        let id = VaultIdentity::generate();
        let file = sealed_to(&id, &vec![7u8; CHUNK + 100]);
        let (_, _, payload) = parts(&file);
        let first_end = payload + 16 + CHUNK + TAG; // 16-byte nonce first
        for i in [payload + 16, first_end - 1, first_end, file.len() - 1] {
            let mut bad = file.clone();
            bad[i] ^= 0x80;
            assert!(decrypt_with_identity(&bad, &id).is_err(), "flip at {i}");
        }
        for len in [first_end, first_end + 1, file.len() - 1] {
            assert!(
                decrypt_with_identity(&file[..len], &id).is_err(),
                "len {len}"
            );
        }
    }

    /// Byte offsets of the parts of an age file: (stanza body start,
    /// "---" MAC line start, payload start).
    fn parts(file: &[u8]) -> (usize, usize, usize) {
        let find = |needle: &[u8]| {
            file.windows(needle.len())
                .position(|w| w == needle)
                .unwrap()
        };
        let stanza = find(b"\n-> ") + 1;
        let body = stanza + file[stanza..].iter().position(|b| *b == b'\n').unwrap() + 1;
        let mac = find(b"\n--- ") + 1;
        let payload = mac + file[mac..].iter().position(|b| *b == b'\n').unwrap() + 1;
        (body, mac, payload)
    }

    // A passphrase file: flips in the version line, the stanza, the MAC,
    // the nonce and the payload, and truncation. Each try runs scrypt, so
    // this samples positions instead of trying all of them.
    #[test]
    fn passphrase_file_bit_flips_and_truncation_are_refused() {
        let file = sealed_with("pw", b"identity text");
        let (body, mac, payload) = parts(&file);
        let at = [
            0,
            5,
            body,
            body + 10,
            mac + 4,
            mac + 20,
            payload,
            payload + 16,
            file.len() - 1,
        ];
        for i in at {
            let mut bad = file.clone();
            bad[i] ^= 0x01;
            assert!(
                decrypt_with_passphrase(&bad, &pass("pw")).is_err(),
                "flip at {i}"
            );
        }
        for len in [0, body, mac, payload, payload + 16, file.len() - 1] {
            let err = decrypt_with_passphrase(&file[..len], &pass("pw"));
            assert!(err.is_err(), "len {len}");
        }
    }

    // age's rule: a scrypt stanza must be the only stanza in the file, so
    // a passphrase file can't also be opened by a key (and a key file
    // can't gain a passphrase). age refuses such a header while parsing,
    // before any key is tried, even for the right key.
    #[test]
    fn scrypt_mixed_with_another_stanza_is_refused() {
        let id = VaultIdentity::generate();
        let by_pass = sealed_with("pw", b"x");
        let by_key = sealed_to(&id, b"x");
        let (_, key_mac, _) = parts(&by_key);
        let key_stanza_start = parts(&by_key).0;
        let line_start = by_key[..key_stanza_start - 1]
            .iter()
            .rposition(|b| *b == b'\n')
            .unwrap()
            + 1;
        let key_stanza = &by_key[line_start..key_mac];
        let (_, mac, _) = parts(&by_pass);
        let mixed = [&by_pass[..mac], key_stanza, &by_pass[mac..]].concat();

        let err = decrypt_with_passphrase(&mixed, &pass("pw")).unwrap_err();
        assert_eq!(err, CryptoError::Corrupt);
        let err = decrypt_with_identity(&mixed, &id).unwrap_err();
        assert_eq!(err, CryptoError::Corrupt);
    }

    // A planted file with scrypt log N = 40 would take hours; it is refused
    // before any scrypt work.
    #[test]
    fn excessive_scrypt_work_factor_is_refused() {
        let file = sealed_with("pw", b"x");
        let (body, _, _) = parts(&file);
        let line = String::from_utf8(file[..body].to_vec()).unwrap();
        let raised = line.replacen(&format!(" {TEST_SCRYPT_LOG_N}\n"), " 40\n", 1);
        assert_ne!(line, raised);
        let bad = [raised.as_bytes(), &file[body..]].concat();
        let err = decrypt_with_passphrase(&bad, &pass("pw")).unwrap_err();
        assert_eq!(err, CryptoError::WorkFactorTooHigh);
    }

    #[test]
    fn size_limits_are_checked_first() {
        let id = VaultIdentity::generate();
        let huge = vec![0u8; MAX_CIPHERTEXT + 1];
        let err = decrypt_with_identity(&huge, &id).unwrap_err();
        assert_eq!(err, CryptoError::TooLarge);
        let err = decrypt_with_passphrase(&huge, &pass("pw")).unwrap_err();
        assert_eq!(err, CryptoError::TooLarge);
        let err = encrypt_to_recipients(&huge[..MAX_PLAINTEXT + 1], &[id.recipient()]).err();
        assert_eq!(err, Some(CryptoError::TooLarge));

        // The largest plaintext fits within MAX_CIPHERTEXT.
        let max = encrypt_to_recipients(&huge[..MAX_PLAINTEXT], &[id.recipient()]).unwrap();
        assert!(max.0.len() <= MAX_CIPHERTEXT);
        assert_eq!(
            decrypt_with_identity(&max.0, &id).unwrap().len(),
            MAX_PLAINTEXT
        );
    }

    #[test]
    fn error_messages_are_fixed_text() {
        let file = sealed_with("pw", b"secret");
        let err = decrypt_with_passphrase(&file, &pass("hunter2")).unwrap_err();
        assert_eq!(err.to_string(), "wrong passphrase");
        assert!(!format!("{err:?}").contains("hunter2"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn arbitrary_plaintext_round_trips(secret in proptest::collection::vec(any::<u8>(), 0..200_000)) {
            let id = VaultIdentity::generate();
            let file = sealed_to(&id, &secret);
            prop_assert_eq!(&decrypt_with_identity(&file, &id).unwrap()[..], &secret[..]);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4))]

        #[test]
        fn arbitrary_plaintext_round_trips_by_passphrase(secret in proptest::collection::vec(any::<u8>(), 0..5_000)) {
            let file = sealed_with("pw", &secret);
            let plain = decrypt_with_passphrase(&file, &pass("pw")).unwrap();
            prop_assert_eq!(&plain[..], &secret[..]);
        }
    }
}
