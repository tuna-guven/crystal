//! Crypto boundary: the age identity and encrypt/decrypt of the vault blob.
//! Plaintext above this module, ciphertext below.
//!
//! Everything goes through Crystal's own implementation of the age v1
//! format (`crate::age_v1`, decision 0042); the files are plain age files.
//! This module adds four things on top: Crystal's scrypt policy (decision
//! 0027), size limits checked before any allocation, error values that
//! carry no text from the input, and types that keep secrets from reaching
//! Debug, Display, Clone or serde.

use std::fmt;

use rustix::io::Errno;
use rustix::rand::{GetRandomFlags, getrandom};
use zeroize::{Zeroize, Zeroizing};

use crate::age_v1::{self, x25519};

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

/// The fewest characters a new passphrase may have
/// (docs/decisions/0028-passphrase-minimum.md). With scrypt at log N 18,
/// this is what keeps offline guessing of a copied `identity.age` out of
/// reach.
pub const MIN_NEW_PASSPHRASE_CHARS: usize = 20;

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

/// What `fitting_log_n` returns for an unlimited RLIMIT_MEMLOCK (2^30
/// needs 1 TiB); `policy_for` caps decryption far below it.
const MAX_SCRYPT_LOG_N: u8 = 30;

/// scrypt work factors for this run (docs/decisions/0027-scrypt-fixed.md).
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

/// log2 N for every passphrase file Crystal writes, on every device: the
/// largest whose memory fits in half of `harden::MEMLOCK_FLOOR` (256 MiB),
/// so any device that starts Crystal can open it. Fixed, not timed:
/// `identity.age` is synced, and a factor picked by a faster or larger
/// machine could be refused elsewhere (docs/decisions/0027-scrypt-fixed.md).
const ENCRYPT_LOG_N: u8 = 18;

/// The scrypt policy for a run with memlock hard limit `memlock`.
/// Decryption accepts up to 16 times Crystal's own cost (age's allowance,
/// so files written before 0027 at log N 19 to 22 still open), and never
/// more than fits in half the limit, where scrypt would abort the process.
fn policy_for(memlock: Option<u64>) -> ScryptPolicy {
    ScryptPolicy {
        encrypt: ENCRYPT_LOG_N,
        max_decrypt: (ENCRYPT_LOG_N + 4).min(fitting_log_n(memlock)),
    }
}

/// This run's policy: `policy_for` the memlock hard limit, read once.
/// Test builds use log N 10 instead, so the unit tests run in
/// milliseconds; the binary has no way to lower the factor.
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
            policy_for(rustix::process::getrlimit(rustix::process::Resource::Memlock).maximum)
        })
    }
}

/// Why encryption or decryption failed. Fixed variants only: file
/// contents and passphrases never reach a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The scrypt stanza did not open with this passphrase. A bit flip in
    /// that stanza looks exactly the same: age's format cannot tell them
    /// apart.
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
    /// Encryption failed: more recipients than an age header Crystal reads
    /// may hold (cannot happen with Crystal's one vault recipient).
    EncryptFailed,
    /// The kernel's random generator failed while making a key, nonce or
    /// salt (age's `OsRng` panicked there).
    RandomFailed,
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
            Self::RandomFailed => "cannot get random bytes from the kernel",
        })
    }
}

impl std::error::Error for CryptoError {}

/// Bytes produced by age encryption (`crate::age_v1`). The field is private and the only
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

/// A passphrase: non-empty UTF-8 (what age tools accept, so a passphrase
/// file can be opened with age or rage too), in a buffer wiped on drop.
/// Deliberately no Debug, Display, Clone or Serialize.
pub struct Passphrase(Zeroizing<Vec<u8>>);

assert_not_impl!(Passphrase: fmt::Debug, fmt::Display, serde::Serialize, Clone);

impl Passphrase {
    /// None for an empty or non-UTF-8 passphrase.
    pub fn new(bytes: Zeroizing<Vec<u8>>) -> Option<Self> {
        let valid = !bytes.is_empty() && std::str::from_utf8(&bytes).is_ok();
        valid.then_some(Self(bytes))
    }

    /// The bytes scrypt takes: the UTF-8 encoding, as age uses it. No copy.
    fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Whether this passphrase is long enough for a new one: at least
    /// `MIN_NEW_PASSPHRASE_CHARS` characters (Unicode scalar values, not
    /// bytes, so a passphrase in a non-Latin script needs as many
    /// characters as one in ASCII). Only new passphrases are checked.
    pub fn is_long_enough(&self) -> bool {
        std::str::from_utf8(&self.0)
            .is_ok_and(|text| text.chars().count() >= MIN_NEW_PASSPHRASE_CHARS)
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

/// The vault's X25519 identity. Only ever on the heap; the key type
/// (x25519-dalek's `StaticSecret`, `zeroize` feature on) wipes itself when
/// dropped. It is never serialized except into an age encryption call
/// here, and has no Debug, Display, Clone or Serialize.
///
/// Not a `secrecy::SecretBox` as #8 first proposed (decision 0013): the
/// identity wipes itself on drop, which is what `SecretBox` would add.
pub struct VaultIdentity(Box<x25519::Identity>);

assert_not_impl!(VaultIdentity: fmt::Debug, fmt::Display, serde::Serialize, Clone);

/// The vault's recipient (public key), kept for the session so each save
/// can seal `vault.age` after the identity is wiped. Private data
/// (docs/decisions/0044-vault-recipient-private.md): no Debug, Display,
/// Clone or Serialize, so it cannot reach a message, a log or a file. The
/// key never leaves this type: the only thing it does is encrypt. Wiped on
/// drop (best effort, like every wiped value: a move may leave a copy).
pub struct VaultRecipient(Box<x25519::Recipient>);

assert_not_impl!(VaultRecipient: fmt::Debug, fmt::Display, serde::Serialize, Clone);

impl VaultRecipient {
    /// Encrypts `secret` to the vault's recipient, for `vault.age`.
    pub fn encrypt(&self, secret: &[u8]) -> Result<Ciphertext, CryptoError> {
        encrypt_to_recipients(secret, std::slice::from_ref(&self.0))
    }
}

impl Drop for VaultRecipient {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl VaultIdentity {
    /// A new random identity, from the kernel's generator (getrandom).
    pub fn generate() -> Result<Self, CryptoError> {
        x25519::Identity::generate()
            .map(|identity| Self(Box::new(identity)))
            .map_err(|_| CryptoError::RandomFailed)
    }

    /// The public key `vault.age` is encrypted to.
    pub fn recipient(&self) -> x25519::Recipient {
        self.0.to_public()
    }

    /// The recipient to keep for saving once the identity is wiped.
    pub fn vault_recipient(&self) -> VaultRecipient {
        VaultRecipient(Box::new(self.0.to_public()))
    }

    /// `identity.age` (sealed with the passphrase) and `identity.backup.age`
    /// (sealed with the recovery code): the identity's age string
    /// (`AGE-SECRET-KEY-1…`), encrypted to the passphrase alone.
    pub fn seal_with_passphrase(&self, pass: &Passphrase) -> Result<Ciphertext, CryptoError> {
        let secret = self.0.to_secret_string();
        encrypt_to_passphrase(secret.as_bytes(), pass)
    }

    /// Unlock: decrypts `identity.age` and parses the identity. Content
    /// that decrypts but is not an identity is `Corrupt`.
    pub fn open_with_passphrase(ciphertext: &[u8], pass: &Passphrase) -> Result<Self, CryptoError> {
        let secret = decrypt_with_passphrase(ciphertext, pass)?;
        let text = std::str::from_utf8(&secret).map_err(|_| CryptoError::Corrupt)?;
        let identity = x25519::Identity::parse(text).ok_or(CryptoError::Corrupt)?;
        Ok(Self(Box::new(identity)))
    }
}

/// Encrypts `secret` to a passphrase: one scrypt stanza as the only
/// recipient, as age requires, at this run's work factor.
pub fn encrypt_to_passphrase(secret: &[u8], pass: &Passphrase) -> Result<Ciphertext, CryptoError> {
    let recipients = age_v1::Recipients::Passphrase {
        passphrase: pass.as_bytes(),
        log_n: scrypt_policy().encrypt,
    };
    encrypt(&recipients, secret)
}

/// Encrypts `secret` to X25519 recipients: the vault recipient for
/// `vault.age`.
pub fn encrypt_to_recipients(
    secret: &[u8],
    recipients: &[x25519::Recipient],
) -> Result<Ciphertext, CryptoError> {
    if recipients.is_empty() {
        return Err(CryptoError::NoRecipients);
    }
    encrypt(&age_v1::Recipients::X25519(recipients), secret)
}

fn encrypt(recipients: &age_v1::Recipients<'_>, secret: &[u8]) -> Result<Ciphertext, CryptoError> {
    if secret.len() > MAX_PLAINTEXT {
        return Err(CryptoError::TooLarge);
    }
    age_v1::encrypt(recipients, secret)
        .map(Ciphertext)
        .map_err(|e| map_error(e, Expect::Key))
}

/// Decrypts a passphrase-encrypted file. A file with any other stanza is
/// `NotForThisKey` (or `Corrupt` if it mixes scrypt with other stanzas,
/// which age forbids).
pub fn decrypt_with_passphrase(
    ciphertext: &[u8],
    pass: &Passphrase,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    // A file asking for more than this run allows is refused before any
    // scrypt work: more could exceed locked memory and abort.
    let identity = age_v1::Identity::Passphrase {
        passphrase: pass.as_bytes(),
        max_log_n: scrypt_policy().max_decrypt,
    };
    decrypt(ciphertext, &identity, Expect::Passphrase)
}

/// Decrypts a file encrypted to `identity` (`vault.age`).
pub fn decrypt_with_identity(
    ciphertext: &[u8],
    identity: &VaultIdentity,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    decrypt(
        ciphertext,
        &age_v1::Identity::X25519(&identity.0),
        Expect::Key,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Passphrase,
    Key,
}

fn decrypt(
    ciphertext: &[u8],
    identity: &age_v1::Identity<'_>,
    expect: Expect,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if ciphertext.len() > MAX_CIPHERTEXT {
        return Err(CryptoError::TooLarge);
    }
    // The plaintext comes back in a zeroizing buffer of exact size; nothing
    // is returned unless the whole file authenticated.
    age_v1::decrypt(identity, ciphertext).map_err(|e| map_error(e, expect))
}

/// age's error to Crystal's. Exhaustive, so a new age error needs a
/// decision here. A passphrase identity finds no stanza in a key file, and
/// a key finds none in a passphrase file: both are `NotForThisKey`.
fn map_error(e: age_v1::Error, expect: Expect) -> CryptoError {
    use age_v1::Error as E;
    match e {
        E::DecryptionFailed => match expect {
            Expect::Passphrase => CryptoError::WrongPassphrase,
            Expect::Key => CryptoError::NotForThisKey,
        },
        E::NoMatch => CryptoError::NotForThisKey,
        E::ExcessiveWork => CryptoError::WorkFactorTooHigh,
        E::InvalidHeader | E::InvalidMac | E::InvalidPayload => CryptoError::Corrupt,
        E::TooLarge => CryptoError::TooLarge,
        E::NoRecipients => CryptoError::NoRecipients,
        E::TooManyRecipients => CryptoError::EncryptFailed,
        E::Random => CryptoError::RandomFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::age_v1::rage;
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

    // decision 0027: one fixed factor, the largest the floor holds.
    #[test]
    fn encryption_factor_fits_the_floor() {
        use crate::harden::MEMLOCK_FLOOR;
        assert_eq!(fitting_log_n(Some(MEMLOCK_FLOOR)), ENCRYPT_LOG_N);
    }

    // Encryption never depends on the device; decryption takes up to
    // 16 times that cost, capped by what fits in half the limit.
    #[test]
    fn policy_is_fixed_for_encryption_and_capped_for_decryption() {
        const MIB: u64 = 1 << 20;
        let policy = |encrypt, max_decrypt| ScryptPolicy {
            encrypt,
            max_decrypt,
        };
        assert_eq!(policy_for(Some(512 * MIB)), policy(18, 18), "the floor");
        assert_eq!(policy_for(Some(2048 * MIB)), policy(18, 20));
        assert_eq!(policy_for(Some(8192 * MIB)), policy(18, 22));
        assert_eq!(policy_for(Some(64 << 30)), policy(18, 22));
        assert_eq!(policy_for(None), policy(18, 22), "unlimited");
    }

    // decision 0028: characters, not bytes; 19 is refused, 20 accepted.
    #[test]
    fn new_passphrase_minimum_counts_characters() {
        let at = |s: &str| pass(s).is_long_enough();
        assert!(!at(&"a".repeat(MIN_NEW_PASSPHRASE_CHARS - 1)));
        assert!(at(&"a".repeat(MIN_NEW_PASSPHRASE_CHARS)));
        // 19 two-byte characters are 38 bytes, still too few characters.
        assert!(!at(&"ü".repeat(MIN_NEW_PASSPHRASE_CHARS - 1)));
        assert!(at(&"ü".repeat(MIN_NEW_PASSPHRASE_CHARS)));
        assert!(at(&"日本語の".repeat(5)));
        assert!(at("correct horse battery staple"));
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
            VaultIdentity::generate().unwrap(),
            VaultIdentity::generate().unwrap(),
            VaultIdentity::generate().unwrap(),
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
        let id = VaultIdentity::generate().unwrap();
        let by_pass = sealed_with("pw", b"x");
        let by_key = sealed_to(&id, b"x");
        let err = decrypt_with_identity(&by_pass, &id).unwrap_err();
        assert_eq!(err, CryptoError::NotForThisKey);
        let err = decrypt_with_passphrase(&by_key, &pass("pw")).unwrap_err();
        assert_eq!(err, CryptoError::NotForThisKey);
    }

    #[test]
    fn vault_identity_seal_and_open() {
        let id = VaultIdentity::generate().unwrap();
        let file = id.seal_with_passphrase(&pass("pw")).unwrap();
        let opened = VaultIdentity::open_with_passphrase(&file.0, &pass("pw")).unwrap();
        assert_eq!(opened.recipient().to_string(), id.recipient().to_string());
        let vault = sealed_to(&id, b"notes");
        assert_eq!(
            &decrypt_with_identity(&vault, &opened).unwrap()[..],
            b"notes"
        );

        // identity.backup.age is sealed the same way, with the recovery
        // code as the passphrase; another passphrase does not open it.
        let backup = id.seal_with_passphrase(&pass("RECOVERYCODE")).unwrap();
        let opened = VaultIdentity::open_with_passphrase(&backup.0, &pass("RECOVERYCODE")).unwrap();
        assert_eq!(opened.recipient().to_string(), id.recipient().to_string());
        let err = VaultIdentity::open_with_passphrase(&backup.0, &pass("pw")).err();
        assert_eq!(err, Some(CryptoError::WrongPassphrase));

        // Decrypts, but is not an identity.
        let junk = sealed_with("pw", b"not a key");
        let err = VaultIdentity::open_with_passphrase(&junk, &pass("pw")).err();
        assert_eq!(err, Some(CryptoError::Corrupt));
    }

    // Every single-bit flip at every byte of a small key-encrypted file,
    // and every truncation, is refused.
    #[test]
    fn key_file_bit_flips_and_truncation_are_refused() {
        let id = VaultIdentity::generate().unwrap();
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
        let id = VaultIdentity::generate().unwrap();
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
        let id = VaultIdentity::generate().unwrap();
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
        let id = VaultIdentity::generate().unwrap();
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

    // What Crystal wrote before step C2, through the age crate 0.11.5
    // (with its grease stanza): rage:: reads tests/corpus/age-rage/,
    // written by that crate before step C3 removed it.

    // Decision 0042: existing vaults open unchanged. identity.age,
    // identity.backup.age and vault.age as old Crystal wrote them through
    // the age crate open here, and give the same key.
    #[test]
    fn opens_files_written_by_the_age_crate() {
        let identity_age = rage::file("scrypt-identity.age");
        let backup_age = rage::file("scrypt-backup.age");
        // To key0, holding one byte: 0 (the generator's pattern).
        let vault_age = rage::file("x25519-1-1.age");
        let key0 = rage::key_strings()[0].1.clone();

        let id =
            VaultIdentity::open_with_passphrase(&identity_age, &pass("correct horse")).unwrap();
        assert_eq!(id.recipient().to_string(), key0);
        let from_backup =
            VaultIdentity::open_with_passphrase(&backup_age, &pass("K7QM3XRD")).unwrap();
        assert_eq!(from_backup.recipient().to_string(), key0);
        assert_eq!(&decrypt_with_identity(&vault_age, &id).unwrap()[..], [0]);

        // Wrong passphrase and wrong key give the same errors as before.
        let err = VaultIdentity::open_with_passphrase(&identity_age, &pass("wrong")).err();
        assert_eq!(err, Some(CryptoError::WrongPassphrase));
        let other = VaultIdentity::generate().unwrap();
        let err = decrypt_with_identity(&vault_age, &other).unwrap_err();
        assert_eq!(err, CryptoError::NotForThisKey);
    }

    // decision 0027: files written at up to 16 times Crystal's own cost
    // (log N + 4, as old age-timed files could be) still open; one more is
    // refused before any scrypt work. The age crate wrote both at "pw".
    #[test]
    fn age_crate_work_factors_up_to_the_cap_open() {
        assert_eq!(TEST_SCRYPT_LOG_N + 4, 14, "the fixtures' work factors");
        let at_cap = rage::file("scrypt-log-n-14.age");
        let plain = decrypt_with_passphrase(&at_cap, &pass("pw")).unwrap();
        assert_eq!(&plain[..], b"x");
        let above = rage::file("scrypt-log-n-15.age");
        let err = decrypt_with_passphrase(&above, &pass("pw")).unwrap_err();
        assert_eq!(err, CryptoError::WorkFactorTooHigh);
    }

    /// The header lines of an age file, read as text without Crystal's
    /// parser: everything up to and including the `---` MAC line.
    fn header_lines(file: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for line in file.split(|&b| b == b'\n') {
            let line = String::from_utf8(line.to_vec()).unwrap();
            let last = line.starts_with("--- ");
            lines.push(line);
            if last {
                return lines;
            }
        }
        panic!("no MAC line");
    }

    // What Crystal writes has the layout age writes, so age and rage
    // remain a way into a vault. That age reads it rests on
    // age_v1::rage: under the same file key, Crystal's code re-encodes
    // age's own files byte for byte; and decision 0042's CCTV vectors.
    #[test]
    fn crystal_writes_what_age_reads() {
        let (age_secret, age_recipient) = rage::key_strings().swap_remove(0);
        let id = VaultIdentity::generate().unwrap();

        // identity.age: the version line, one scrypt stanza (a 16-byte
        // salt, the work factor) with its 32-byte body, the MAC; no
        // grease, as Go age writes it.
        let identity_age = id.seal_with_passphrase(&pass("correct horse")).unwrap();
        let lines = header_lines(identity_age.as_bytes());
        assert_eq!(lines.len(), 4, "{} header lines", lines.len());
        assert_eq!(lines[0], "age-encryption.org/v1");
        let args: Vec<&str> = lines[1].split(' ').collect();
        assert_eq!(args[..2], ["->", "scrypt"]);
        assert_eq!((args.len(), args[2].len()), (4, 22), "salt");
        assert_eq!(args[3], TEST_SCRYPT_LOG_N.to_string());
        assert_eq!(lines[2].len(), 43, "file key and tag, base64");
        assert_eq!(lines[3].len(), 4 + 43, "--- and the MAC");

        // It holds the identity as an age secret key string, of the same
        // form as the one age printed.
        let text =
            decrypt_with_passphrase(identity_age.as_bytes(), &pass("correct horse")).unwrap();
        let text = std::str::from_utf8(&text).unwrap();
        assert!(text.starts_with("AGE-SECRET-KEY-1"));
        assert_eq!(text.len(), age_secret.len());
        assert!(!text.bytes().any(|b| b.is_ascii_lowercase()));

        // vault.age: the version line, one X25519 stanza (a 32-byte share)
        // with its 32-byte body, the MAC; no grease.
        let vault = encrypt_to_recipients(b"notes", &[id.recipient()]).unwrap();
        let lines = header_lines(vault.as_bytes());
        assert_eq!(lines.len(), 4, "{} header lines", lines.len());
        assert_eq!(lines[0], "age-encryption.org/v1");
        let args: Vec<&str> = lines[1].split(' ').collect();
        assert_eq!((args[..2].to_vec(), args.len()), (vec!["->", "X25519"], 3));
        assert_eq!(args[2].len(), 43, "share, base64");
        assert_eq!(lines[2].len(), 43, "file key and tag, base64");
        assert_eq!(lines[3].len(), 4 + 43, "--- and the MAC");
        assert_eq!(
            &decrypt_with_identity(vault.as_bytes(), &id).unwrap()[..],
            b"notes"
        );
        let recipient = id.recipient().to_string();
        assert!(recipient.starts_with("age1"));
        assert_eq!(recipient.len(), age_recipient.len());
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
            let id = VaultIdentity::generate().unwrap();
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
