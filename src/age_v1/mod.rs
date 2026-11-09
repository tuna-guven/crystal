//! Crystal's own implementation of the age v1 file format
//! (https://github.com/C2SP/C2SP/blob/main/age.md), byte-compatible with
//! age and rage, trimmed to what Crystal needs: scrypt and X25519
//! recipients, whole buffers of at most 32 MiB. No armor, plugins, SSH
//! keys or streaming I/O.
//!
//! Files that port age's code (age 0.12.1, age-core 0.12.0) name their
//! source file in a header; that code is used under the MIT license
//! (LICENSES/age-MIT.txt). Nothing here is a custom construction: every
//! step follows the spec and age's code.
//!
//! Since step C2, `crypto` encrypts and decrypts through this module.
//! Since step C3 the `age` crate is gone; the C2SP/CCTV vectors (`cctv`)
//! and files the age crate wrote before then (`rage`,
//! tests/corpus/age-rage/; decision 0043) check it.
//!
//! `encrypt` and `decrypt` follow age 0.12.1, src/protocol.rs
//! (`Encryptor::with_recipients`, `Decryptor`) and src/keys.rs, over
//! whole buffers. A file is the header, a 16-byte random payload nonce,
//! then the STREAM payload. Decryption parses the header, checks that a
//! scrypt stanza stands alone, reads the nonce, finds the file key by
//! trying each stanza in order, verifies the header MAC, and only then
//! touches the payload.

use std::fmt;

use zeroize::Zeroizing;

use crate::model::assert_not_impl;

mod base64;
mod bech32;
#[cfg(test)]
mod cctv;
mod header;
mod primitives;
#[cfg(test)]
pub(crate) mod rage;
mod scrypt;
mod stream;
pub(crate) mod x25519;

/// The length of an age file key (C2SP age: 128 bits).
const FILE_KEY_BYTES: usize = 16;

/// Why an age operation failed. Fixed variants only: no key, passphrase,
/// file or note bytes ever reach an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    /// Not a valid age v1 file: a malformed header or stanza (age's
    /// `InvalidHeader`).
    InvalidHeader,
    /// The scrypt stanza did not open: a wrong passphrase, or a damaged
    /// stanza; the two look the same (age's `DecryptionFailed`).
    DecryptionFailed,
    /// The scrypt stanza asks for a work factor above the allowed maximum
    /// (age's `ExcessiveWork`); refused before any scrypt work.
    ExcessiveWork,
    /// The kernel's random generator failed, or produced an all-zero
    /// X25519 share (age panics there; Crystal returns an error).
    Random,
    /// The header MAC does not match: the header was changed, or the file
    /// key is wrong (age's `InvalidMac`).
    InvalidMac,
    /// The payload after the nonce is missing, truncated, extended or
    /// damaged (age reports these as stream I/O errors).
    InvalidPayload,
    /// No stanza opens with this identity (age's `NoMatchingKeys`).
    NoMatch,
    /// `encrypt` was given no X25519 recipients (age's
    /// `MissingRecipients`).
    NoRecipients,
    /// More recipients than a header Crystal reads may hold
    /// (`header::MAX_STANZAS`).
    TooManyRecipients,
    /// Over `crypto::MAX_PLAINTEXT` or `crypto::MAX_CIPHERTEXT`, checked
    /// before any work or allocation.
    TooLarge,
}

/// Who a file is encrypted to.
pub(crate) enum Recipients<'a> {
    /// A passphrase at scrypt work factor `log_n`: the file's only stanza,
    /// as age requires.
    Passphrase { passphrase: &'a [u8], log_n: u8 },
    /// One or more X25519 public keys.
    X25519(&'a [x25519::Recipient]),
}

/// What a file is decrypted with.
pub(crate) enum Identity<'a> {
    /// A passphrase; a scrypt stanza asking for more than `max_log_n` is
    /// refused before any scrypt work.
    Passphrase { passphrase: &'a [u8], max_log_n: u8 },
    /// An X25519 secret key.
    X25519(&'a x25519::Identity),
}

/// Encrypts `plaintext` to `recipients`: a fresh file key wrapped once per
/// recipient, the header with its MAC, a fresh payload nonce, then the
/// STREAM payload (age's `Encryptor::with_recipients` and `wrap_output`).
pub(crate) fn encrypt(recipients: &Recipients<'_>, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    if plaintext.len() > crate::crypto::MAX_PLAINTEXT {
        return Err(Error::TooLarge);
    }
    let file_key = FileKey::generate()?;
    let stanzas = match recipients {
        Recipients::Passphrase { passphrase, log_n } => {
            vec![scrypt::wrap(&file_key, passphrase, *log_n)?]
        }
        Recipients::X25519([]) => return Err(Error::NoRecipients),
        Recipients::X25519(keys) if keys.len() > header::MAX_STANZAS => {
            return Err(Error::TooManyRecipients);
        }
        Recipients::X25519(keys) => keys
            .iter()
            .map(|key| key.wrap(&file_key))
            .collect::<Result<_, _>>()?,
    };
    // Cannot fail: at most MAX_STANZAS stanzas of a few dozen bytes each.
    let header = header::write(&stanzas, file_key.expose()).ok_or(Error::TooManyRecipients)?;
    let mut nonce = [0; stream::PAYLOAD_NONCE_LEN];
    crate::crypto::fill_random(&mut nonce).map_err(|_| Error::Random)?;
    let payload_key = stream::payload_key(file_key.expose(), &nonce);
    let payload = stream::encrypt(&payload_key, plaintext).ok_or(Error::TooLarge)?;

    let mut file = Vec::with_capacity(header.len() + nonce.len() + payload.len());
    file.extend_from_slice(&header);
    file.extend_from_slice(&nonce);
    file.extend_from_slice(&payload);
    Ok(file)
}

/// Decrypts an age file with `identity` (age's `Decryptor::new_buffered`
/// and `decrypt`). The plaintext is returned in a wiped buffer; nothing of
/// it is returned unless every chunk authenticated.
pub(crate) fn decrypt(identity: &Identity<'_>, file: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if file.len() > crate::crypto::MAX_CIPHERTEXT {
        return Err(Error::TooLarge);
    }
    let (header, rest) = header::parse(file).ok_or(Error::InvalidHeader)?;
    // age's `HeaderV1::is_valid`: a scrypt stanza must be the only one.
    let any_scrypt = header
        .stanzas
        .iter()
        .any(|s| s.tag == scrypt::SCRYPT_RECIPIENT_TAG);
    if any_scrypt && header.stanzas.len() != 1 {
        return Err(Error::InvalidHeader);
    }
    // The nonce is read before any stanza is tried, as age does, so a
    // truncated file costs no scrypt work. A missing or short nonce is a
    // header failure (CCTV `stream_no_nonce`, `stream_short_nonce`).
    let (nonce, payload) = rest
        .split_first_chunk::<{ stream::PAYLOAD_NONCE_LEN }>()
        .ok_or(Error::InvalidHeader)?;

    // The first stanza this identity answers for decides, as age's
    // `find_map` over the stanzas.
    let file_key = header
        .stanzas
        .iter()
        .find_map(|stanza| match identity {
            Identity::Passphrase {
                passphrase,
                max_log_n,
            } => scrypt::unwrap(stanza, passphrase, *max_log_n),
            Identity::X25519(key) => key.unwrap(stanza),
        })
        .unwrap_or(Err(Error::NoMatch))?;

    if !header.verify_mac(file_key.expose()) {
        return Err(Error::InvalidMac);
    }
    let payload_key = stream::payload_key(file_key.expose(), nonce);
    stream::decrypt(&payload_key, payload).ok_or(Error::InvalidPayload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::OnceLock;

    // The test key pair from age 0.12.1, src/native/x25519.rs.
    const TEST_SK: &str =
        "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";
    const TEST_PK: &str = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa";
    const TEST_MSG: &[u8] = b"This is a test message. For testing.";

    fn test_identity() -> x25519::Identity {
        x25519::Identity::parse(TEST_SK).unwrap()
    }

    fn passphrase(p: &[u8], log_n: u8) -> Recipients<'_> {
        Recipients::Passphrase {
            passphrase: p,
            log_n,
        }
    }

    fn with_passphrase(p: &[u8], max_log_n: u8) -> Identity<'_> {
        Identity::Passphrase {
            passphrase: p,
            max_log_n,
        }
    }

    // Ported from age 0.12.1, src/protocol.rs: x25519_round_trip.
    #[test]
    fn x25519_round_trip() {
        let pk = x25519::Recipient::parse(TEST_PK).unwrap();
        let file = encrypt(&Recipients::X25519(&[pk]), TEST_MSG).unwrap();
        let sk = test_identity();
        assert_eq!(*decrypt(&Identity::X25519(&sk), &file).unwrap(), TEST_MSG);
    }

    // Ported: scrypt_round_trip, at age's test work factor 2.
    #[test]
    fn scrypt_round_trip() {
        let file = encrypt(&passphrase(b"passphrase", 2), TEST_MSG).unwrap();
        let back = decrypt(&with_passphrase(b"passphrase", 2), &file).unwrap();
        assert_eq!(*back, TEST_MSG);
    }

    // age's mixed_recipient_and_passphrase and incompatible_recipients test
    // its label check while encrypting; `Recipients` cannot express a mix.
    // Reading, a scrypt stanza next to any other is refused, as age's
    // `HeaderV1::is_valid` does.
    #[test]
    fn scrypt_must_stand_alone() {
        let key = FileKey::generate().unwrap();
        let alone = scrypt::wrap(&key, b"p", 2).unwrap();
        let x = test_identity().to_public().wrap(&key).unwrap();
        let grease = header::Stanza {
            tag: "a-grease".into(),
            args: vec![],
            body: vec![],
        };
        for stanzas in [
            vec![alone.clone(), x.clone()],
            vec![x, alone.clone()],
            vec![alone.clone(), alone.clone()],
            vec![alone, grease],
        ] {
            let mut file = header::write(&stanzas, key.expose()).unwrap();
            file.extend_from_slice(&[0; 16 + 16]);
            for identity in [with_passphrase(b"p", 2), Identity::X25519(&test_identity())] {
                assert_eq!(decrypt(&identity, &file).err(), Some(Error::InvalidHeader));
            }
        }
    }

    #[test]
    fn wrong_key_or_kind() {
        let x_file = encrypt(&Recipients::X25519(&[test_identity().to_public()]), b"x").unwrap();
        let p_file = encrypt(&passphrase(b"p", 2), b"p").unwrap();
        let stranger = x25519::Identity::generate().unwrap();
        assert_eq!(
            decrypt(&Identity::X25519(&stranger), &x_file).err(),
            Some(Error::NoMatch)
        );
        assert_eq!(
            decrypt(&with_passphrase(b"p", 2), &x_file).err(),
            Some(Error::NoMatch)
        );
        assert_eq!(
            decrypt(&Identity::X25519(&stranger), &p_file).err(),
            Some(Error::NoMatch)
        );
        assert_eq!(
            decrypt(&with_passphrase(b"q", 2), &p_file).err(),
            Some(Error::DecryptionFailed)
        );
        assert_eq!(
            decrypt(&with_passphrase(b"p", 1), &p_file).err(),
            Some(Error::ExcessiveWork)
        );
    }

    // The MAC is checked before the payload: a changed header with an
    // intact stanza is InvalidMac even when the payload is garbage too.
    #[test]
    fn mac_is_checked_before_the_payload() {
        let sk = test_identity();
        let mut file = encrypt(&Recipients::X25519(&[sk.to_public()]), TEST_MSG).unwrap();
        let header_len = file.len() - header::parse(&file).unwrap().1.len();
        // The header ends "--- " + 43 MAC characters + LF. Change the MAC's
        // first character to another valid one.
        let i = header_len - 44;
        file[i] = if file[i] == b'A' { b'B' } else { b'A' };
        let end = file.len();
        file[end - 1] ^= 1;
        assert_eq!(
            decrypt(&Identity::X25519(&sk), &file).err(),
            Some(Error::InvalidMac)
        );
    }

    #[test]
    fn short_or_missing_payload() {
        let sk = test_identity();
        let file = encrypt(&Recipients::X25519(&[sk.to_public()]), b"").unwrap();
        let header_len = file.len() - 16 - 16;
        // No nonce or a short one: a header failure, as CCTV expects.
        for cut in [header_len, header_len + 15] {
            assert_eq!(
                decrypt(&Identity::X25519(&sk), &file[..cut]).err(),
                Some(Error::InvalidHeader),
                "{cut}"
            );
        }
        // The nonce but no chunk, or a cut chunk: a payload failure.
        for cut in [header_len + 16, file.len() - 1] {
            assert_eq!(
                decrypt(&Identity::X25519(&sk), &file[..cut]).err(),
                Some(Error::InvalidPayload),
                "{cut}"
            );
        }
        assert!(decrypt(&Identity::X25519(&sk), &file).unwrap().is_empty());
        assert_eq!(
            decrypt(&Identity::X25519(&sk), b"").err(),
            Some(Error::InvalidHeader)
        );
    }

    #[test]
    fn limits() {
        let pk = test_identity().to_public();
        let too_big = vec![0; crate::crypto::MAX_PLAINTEXT + 1];
        assert_eq!(
            encrypt(&Recipients::X25519(std::slice::from_ref(&pk)), &too_big).err(),
            Some(Error::TooLarge)
        );
        let too_big = vec![0; crate::crypto::MAX_CIPHERTEXT + 1];
        assert_eq!(
            decrypt(&Identity::X25519(&test_identity()), &too_big).err(),
            Some(Error::TooLarge)
        );

        assert_eq!(
            encrypt(&Recipients::X25519(&[]), b"").err(),
            Some(Error::NoRecipients)
        );
        let many = vec![pk; header::MAX_STANZAS + 1];
        assert_eq!(
            encrypt(&Recipients::X25519(&many), b"").err(),
            Some(Error::TooManyRecipients)
        );
        let file = encrypt(&Recipients::X25519(&many[..header::MAX_STANZAS]), b"x").unwrap();
        assert_eq!(
            *decrypt(&Identity::X25519(&test_identity()), &file).unwrap(),
            b"x"
        );
    }

    #[test]
    fn several_recipients() {
        let keys: Vec<x25519::Identity> = (0..5)
            .map(|_| x25519::Identity::generate().unwrap())
            .collect();
        let recipients: Vec<_> = keys.iter().map(x25519::Identity::to_public).collect();
        let file = encrypt(&Recipients::X25519(&recipients), TEST_MSG).unwrap();
        for key in &keys {
            assert_eq!(*decrypt(&Identity::X25519(key), &file).unwrap(), TEST_MSG);
        }
    }

    /// Empty, one byte, around one chunk and whole multiples of a chunk.
    /// Files age wrote at these sizes are checked in rage.rs; Crystal's
    /// 32 MiB maximum has its own test below.
    const BOUNDARY_SIZES: [usize; 8] = [
        0,
        1,
        stream::CHUNK_SIZE - 1,
        stream::CHUNK_SIZE,
        stream::CHUNK_SIZE + 1,
        2 * stream::CHUNK_SIZE,
        3 * stream::CHUNK_SIZE,
        3 * stream::CHUNK_SIZE + 5,
    ];

    fn plaintext_of(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 253) as u8).collect()
    }

    /// X25519 to one recipient and to three, then scrypt, at `len` bytes:
    /// every key, and the passphrase, opens what `encrypt` wrote.
    fn round_trip_at(len: usize) {
        let plaintext = plaintext_of(len);
        let keys: Vec<x25519::Identity> = (0..3)
            .map(|_| x25519::Identity::generate().unwrap())
            .collect();
        let recipients: Vec<_> = keys.iter().map(x25519::Identity::to_public).collect();
        for n in [1, 3] {
            let file = encrypt(&Recipients::X25519(&recipients[..n]), &plaintext).unwrap();
            for key in &keys[..n] {
                let back = decrypt(&Identity::X25519(key), &file).unwrap();
                assert!(*back == plaintext, "{len} to {n}");
            }
        }
        let pass = "pässwörd".as_bytes();
        let file = encrypt(&passphrase(pass, 10), &plaintext).unwrap();
        let back = decrypt(&with_passphrase(pass, 10), &file).unwrap();
        assert!(*back == plaintext, "{len} by passphrase");
    }

    #[test]
    fn round_trips_at_chunk_boundaries() {
        for len in BOUNDARY_SIZES {
            round_trip_at(len);
        }
    }

    // 32 MiB (512 chunks) through the unoptimized ChaCha20 of a debug build
    // takes about a minute; release builds (`cargo test --release`,
    // scripts/long-tests.sh) run it in under a second.
    #[test]
    #[cfg_attr(debug_assertions, ignore = "slow in debug builds; runs in release")]
    fn round_trips_32_mib() {
        round_trip_at(crate::crypto::MAX_PLAINTEXT);
    }

    /// Two full chunks and a few bytes, encrypted once to the test key.
    fn sample() -> &'static [u8] {
        static SAMPLE: OnceLock<Vec<u8>> = OnceLock::new();
        SAMPLE.get_or_init(|| {
            encrypt(
                &Recipients::X25519(&[test_identity().to_public()]),
                &[3; 2 * stream::CHUNK_SIZE + 7],
            )
            .unwrap()
        })
    }

    proptest! {
        #[test]
        fn round_trip(plaintext in proptest::collection::vec(any::<u8>(), 0..3000)) {
            let sk = test_identity();
            let file = encrypt(&Recipients::X25519(&[sk.to_public()]), &plaintext).unwrap();
            prop_assert_eq!(&*decrypt(&Identity::X25519(&sk), &file).unwrap(), &plaintext[..]);
        }

        #[test]
        fn flipped_bit_fails(bit in any::<prop::sample::Index>()) {
            let mut file = sample().to_vec();
            let bit = bit.index(file.len() * 8);
            file[bit / 8] ^= 1 << (bit % 8);
            prop_assert!(decrypt(&Identity::X25519(&test_identity()), &file).is_err());
        }

        #[test]
        fn truncation_or_extension_fails(
            cut in any::<prop::sample::Index>(),
            extra in proptest::collection::vec(any::<u8>(), 1..64),
        ) {
            let file = sample();
            let sk = test_identity();
            prop_assert!(decrypt(&Identity::X25519(&sk), &file[..cut.index(file.len())]).is_err());
            prop_assert!(decrypt(&Identity::X25519(&sk), &[file, &extra[..]].concat()).is_err());
        }

        #[test]
        fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let sk = test_identity();
            prop_assert!(decrypt(&Identity::X25519(&sk), &bytes).is_err());
            let prefixed = [&b"age-encryption.org/v1\n-> X25519 "[..], &bytes[..]].concat();
            prop_assert!(decrypt(&Identity::X25519(&sk), &prefixed).is_err());
            prop_assert!(decrypt(&with_passphrase(b"p", 2), &prefixed).is_err());
        }
    }
}

/// An age file key: 16 bytes on the heap (as age's `SecretBox`), filled in
/// place and wiped on drop, so moving a `FileKey` copies only a pointer.
/// No Debug, Display or Clone, so it cannot be printed or copied by
/// accident.
pub(super) struct FileKey(Box<Zeroizing<[u8; FILE_KEY_BYTES]>>);

assert_not_impl!(FileKey: fmt::Debug, fmt::Display, Clone);

impl FileKey {
    fn zeroed() -> Self {
        Self(Box::new(Zeroizing::new([0; FILE_KEY_BYTES])))
    }

    /// A new random file key from the kernel (getrandom, no file access).
    pub(super) fn generate() -> Result<Self, Error> {
        let mut key = Self::zeroed();
        crate::crypto::fill_random(&mut **key.0).map_err(|_| Error::Random)?;
        Ok(key)
    }

    /// The key from an unwrapped stanza body. None unless exactly 16 bytes.
    fn from_slice(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != FILE_KEY_BYTES {
            return None;
        }
        let mut key = Self::zeroed();
        key.0.copy_from_slice(bytes);
        Some(key)
    }

    pub(super) fn expose(&self) -> &[u8; FILE_KEY_BYTES] {
        &self.0
    }
}
