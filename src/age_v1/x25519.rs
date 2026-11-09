//! The X25519 recipient (C2SP age, "The X25519 recipient type").
//!
//! Derived from age 0.12.1, src/native/x25519.rs
//! (https://github.com/str4d/rage), used under the MIT license:
//! Copyright (c) 2019 Jack Grigg (LICENSES/age-MIT.txt).
//!
//! The stanza is `-> X25519 <base64 ephemeral share>` with the wrapped
//! file key as its body. The wrapping key is HKDF-SHA-256(salt = ephemeral
//! share || recipient, label "age-encryption.org/v1/X25519", the X25519
//! shared secret). Keys are written `age1…` (public) and
//! `AGE-SECRET-KEY-1…` (secret) in Bech32.
//!
//! Changes for Crystal: secret and ephemeral scalars are 32 bytes from the
//! kernel's getrandom turned into a `StaticSecret` (age uses
//! `random_from_rng(OsRng)`; x25519-dalek clamps both the same way, so the
//! keys and output are the same kind). An all-zero shared secret while
//! encrypting is an error, not a panic. Key strings use Crystal's
//! zeroizing Bech32 (`super::bech32`).

use std::fmt;

use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};
use zeroize::Zeroizing;

use super::header::Stanza;
use super::primitives::{TAG_LEN, aead_decrypt, aead_encrypt, hkdf};
use super::{Error, FILE_KEY_BYTES, FileKey, base64, bech32};

/// Bech32 human-readable parts, lowercase (`bech32::decode` returns them
/// lowercase; the secret key is shown upper case).
const SECRET_KEY_PREFIX: &str = "age-secret-key-";
const PUBLIC_KEY_PREFIX: &str = "age";

pub(super) const X25519_RECIPIENT_TAG: &str = "X25519";
const X25519_RECIPIENT_KEY_LABEL: &[u8] = b"age-encryption.org/v1/X25519";

const EPK_LEN_BYTES: usize = 32;
const ENCRYPTED_FILE_KEY_BYTES: usize = FILE_KEY_BYTES + TAG_LEN;

/// A random X25519 scalar from the kernel, in a wiped buffer.
fn random_secret() -> Result<StaticSecret, Error> {
    let mut bytes = Zeroizing::new([0; 32]);
    crate::crypto::fill_random(&mut *bytes).map_err(|_| Error::Random)?;
    Ok(StaticSecret::from(*bytes))
}

/// Whether the shared secret is all zeros (a low-order point), checked in
/// constant time, as age does.
fn is_all_zero(shared: &SharedSecret) -> bool {
    shared
        .as_bytes()
        .iter()
        .fold(0, |acc, b| acc | b)
        .ct_eq(&0)
        .into()
}

/// The stanza wrapping key: HKDF(epk || pk, label, shared secret).
fn wrapping_key(epk: &PublicKey, pk: &PublicKey, shared: &SharedSecret) -> Zeroizing<[u8; 32]> {
    let mut salt = [0; 64];
    salt[..32].copy_from_slice(epk.as_bytes());
    salt[32..].copy_from_slice(pk.as_bytes());
    hkdf(&salt, X25519_RECIPIENT_KEY_LABEL, shared.as_bytes())
}

/// An age X25519 identity (secret key). x25519-dalek wipes it on drop.
/// No Debug, Display or Clone.
pub(crate) struct Identity(StaticSecret);

impl Identity {
    /// A new random identity.
    pub(crate) fn generate() -> Result<Self, Error> {
        random_secret().map(Self)
    }

    /// Parses `AGE-SECRET-KEY-1…` (either case, not mixed). None for
    /// anything else, including a recipient string.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        let (hrp, bytes) = bech32::decode(s)?;
        if hrp != SECRET_KEY_PREFIX {
            return None;
        }
        let bytes: &[u8; 32] = bytes.as_slice().try_into().ok()?;
        Some(Self(StaticSecret::from(*bytes)))
    }

    /// The key as age writes it: `AGE-SECRET-KEY-1…`, upper case, in a
    /// wiped buffer.
    pub(crate) fn to_secret_string(&self) -> Zeroizing<String> {
        let bytes = Zeroizing::new(self.0.to_bytes());
        let mut encoded = bech32::encode(SECRET_KEY_PREFIX, &*bytes);
        // In place: no second copy, unlike age's `to_uppercase()`.
        encoded.make_ascii_uppercase();
        encoded
    }

    /// The matching recipient.
    pub(crate) fn to_public(&self) -> Recipient {
        Recipient(PublicKey::from(&self.0))
    }

    /// Tries to unwrap `stanza`, as age's `unwrap_stanza`: None for another
    /// stanza type or for an X25519 stanza that is not for this key (age
    /// cannot tell which stanza is whose, so the caller tries the next);
    /// an error for a malformed X25519 stanza.
    pub(super) fn unwrap(&self, stanza: &Stanza) -> Option<Result<FileKey, Error>> {
        if stanza.tag != X25519_RECIPIENT_TAG {
            return None;
        }
        // Enforce valid and canonical stanza format
        // (c2sp.org/age#x25519-recipient-stanza).
        let [share] = &stanza.args[..] else {
            return Some(Err(Error::InvalidHeader));
        };
        let Some(share) =
            base64::decode(share.as_bytes()).and_then(|b| <[u8; EPK_LEN_BYTES]>::try_from(b).ok())
        else {
            return Some(Err(Error::InvalidHeader));
        };
        if stanza.body.len() != ENCRYPTED_FILE_KEY_BYTES {
            return Some(Err(Error::InvalidHeader));
        }

        let epk = PublicKey::from(share);
        let pk = PublicKey::from(&self.0);
        let shared = self.0.diffie_hellman(&epk);
        if is_all_zero(&shared) {
            return Some(Err(Error::InvalidHeader));
        }
        let key = wrapping_key(&epk, &pk, &shared);
        // A failure to decrypt is not fatal: another stanza may be ours.
        let file_key = aead_decrypt(&key, FILE_KEY_BYTES, &stanza.body)?;
        FileKey::from_slice(&file_key).map(Ok)
    }
}

/// An age X25519 recipient (public key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recipient(PublicKey);

impl Recipient {
    /// Parses `age1…` (either case, not mixed).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        let (hrp, bytes) = bech32::decode(s)?;
        if hrp != PUBLIC_KEY_PREFIX {
            return None;
        }
        let bytes: [u8; 32] = bytes.as_slice().try_into().ok()?;
        Some(Self(PublicKey::from(bytes)))
    }

    /// Wraps `file_key` to this recipient with a fresh ephemeral key.
    pub(super) fn wrap(&self, file_key: &FileKey) -> Result<Stanza, Error> {
        let esk = random_secret()?;
        let epk = PublicKey::from(&esk);
        let shared = esk.diffie_hellman(&self.0);
        // From age: "It is vanishingly unlikely that we generate the
        // all-zero esk, so if we do then it is likely that the RNG is bad".
        // age panics; this returns an error.
        if is_all_zero(&shared) {
            return Err(Error::Random);
        }
        let key = wrapping_key(&epk, &self.0, &shared);
        // Cannot fail for 16 bytes (ChaCha20's limit is 256 GiB).
        let body = aead_encrypt(&key, file_key.expose()).ok_or(Error::Random)?;
        Ok(Stanza {
            tag: X25519_RECIPIENT_TAG.to_owned(),
            args: vec![base64::encode(epk.as_bytes())],
            body,
        })
    }
}

/// `age1…`, lowercase.
impl fmt::Display for Recipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bech32::encode(PUBLIC_KEY_PREFIX, self.0.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::age_v1::header;
    use proptest::prelude::*;
    use secrecy::ExposeSecret;
    use std::io::Write;

    // The test key pair from age 0.12.1, src/native/x25519.rs.
    const TEST_SK: &str =
        "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";
    const TEST_PK: &str = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa";

    // Ported: pubkey_encoding.
    #[test]
    fn pubkey_encoding() {
        assert_eq!(Recipient::parse(TEST_PK).unwrap().to_string(), TEST_PK);
    }

    // Ported: pubkey_from_secret_key.
    #[test]
    fn pubkey_from_secret_key() {
        let key = Identity::parse(TEST_SK).unwrap();
        assert_eq!(key.to_public().to_string(), TEST_PK);
        assert_eq!(*key.to_secret_string(), TEST_SK);
    }

    #[test]
    fn key_strings_are_typed() {
        assert!(Identity::parse(TEST_PK).is_none());
        assert!(Recipient::parse(TEST_SK).is_none());
        assert!(Recipient::parse(&TEST_PK.to_uppercase()).is_some());
        assert!(Identity::parse(&TEST_SK.to_lowercase()).is_some());
        // Right prefix, wrong length.
        assert!(Recipient::parse(&bech32::encode("age", &[1; 31])).is_none());
        assert!(Recipient::parse(&bech32::encode("age", &[1; 33])).is_none());
        assert!(Identity::parse(&bech32::encode("age-secret-key-", &[1; 31])).is_none());
        // A changed character.
        assert!(Recipient::parse(&TEST_PK.replace("t7r", "t7s")).is_none());
    }

    fn identity_from(bytes: &[u8]) -> Identity {
        let mut sk = [0; 32];
        sk[..bytes.len()].copy_from_slice(bytes);
        Identity(StaticSecret::from(sk))
    }

    #[test]
    fn not_ours_is_none() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let stanza = a.to_public().wrap(&FileKey::generate().unwrap()).unwrap();
        assert!(b.unwrap(&stanza).is_none());
        let mut other = stanza.clone();
        other.tag = "x25519".into();
        assert!(a.unwrap(&other).is_none());
    }

    // c2sp.org/age#x25519-recipient-stanza: exactly one argument, a
    // canonical 32-byte share, a 32-byte body, and no all-zero shared
    // secret. Each is a header error.
    #[test]
    fn malformed_stanzas_are_header_errors() {
        let identity = Identity::generate().unwrap();
        let good = identity
            .to_public()
            .wrap(&FileKey::generate().unwrap())
            .unwrap();
        let share = good.args[0].as_str();
        let with_args = |args: &[&str]| Stanza {
            args: args.iter().map(|&a| a.into()).collect(),
            ..good.clone()
        };
        let zero = base64::encode(&[0; 32]);
        let mut non_canonical = share.to_owned();
        non_canonical.pop();
        non_canonical.push('B');
        for stanza in [
            with_args(&[]),
            with_args(&[share, share]),
            with_args(&[&base64::encode(&[1; 31])]),
            with_args(&[&base64::encode(&[1; 33])]),
            with_args(&[&format!("{share}=")]),
            // A low-order point: the shared secret is all zeros.
            with_args(&[&zero]),
            Stanza {
                body: good.body[..31].to_vec(),
                ..good.clone()
            },
            Stanza {
                body: [&good.body[..], &[0]].concat(),
                ..good.clone()
            },
        ] {
            assert_eq!(
                identity.unwrap(&stanza).unwrap().err(),
                Some(Error::InvalidHeader),
                "{:?}",
                stanza.args
            );
        }
        // The last base64 character of a 32-byte share carries 2 padding
        // bits, which must be zero; 'B' sets one.
        assert_eq!(
            identity
                .unwrap(&with_args(&[&non_canonical]))
                .unwrap()
                .err(),
            Some(Error::InvalidHeader)
        );
    }

    // Cross-checks against the age crate (the dependency, 0.11.5; its
    // X25519 code is unchanged in 0.12.1 apart from the Bech32 plumbing).
    #[test]
    fn key_strings_match_age_crate() {
        for _ in 0..50 {
            // age's identity, read here.
            let theirs = age::x25519::Identity::generate();
            let secret = theirs.to_string();
            let ours = Identity::parse(secret.expose_secret()).unwrap();
            assert_eq!(*ours.to_secret_string(), *secret.expose_secret());
            assert_eq!(ours.to_public().to_string(), theirs.to_public().to_string());

            // Ours, read by age.
            let ours = Identity::generate().unwrap();
            let theirs: age::x25519::Identity = ours.to_secret_string().parse().unwrap();
            assert_eq!(
                *theirs.to_string().expose_secret(),
                *ours.to_secret_string()
            );
            let recipient: age::x25519::Recipient = ours.to_public().to_string().parse().unwrap();
            assert_eq!(recipient.to_string(), ours.to_public().to_string());
        }
    }

    // A file the age crate encrypts to several recipients, ours among
    // them: our identity finds its stanza, and the file key verifies the
    // header MAC (the MAC covers every stanza, so a wrong key fails).
    #[test]
    fn opens_age_crate_x25519_files() {
        for position in 0..3 {
            let ours = Identity::generate().unwrap();
            let mut recipients: Vec<age::x25519::Recipient> = (0..2)
                .map(|_| age::x25519::Identity::generate().to_public())
                .collect();
            recipients.insert(position, ours.to_public().to_string().parse().unwrap());
            let encryptor = age::Encryptor::with_recipients(
                recipients.iter().map(|r| r as &dyn age::Recipient),
            )
            .unwrap();
            let mut file = Vec::new();
            let mut writer = encryptor.wrap_output(&mut file).unwrap();
            writer.write_all(b"note").unwrap();
            writer.finish().unwrap();

            let (parsed, _) = header::parse(&file).unwrap();
            assert_eq!(parsed.stanzas.len(), 4, "three X25519 and one grease");
            let mut found = parsed.stanzas.iter().filter_map(|s| ours.unwrap(s));
            let key = found.next().unwrap().unwrap();
            assert!(found.next().is_none());
            assert!(parsed.verify_mac(key.expose()));
            // An identity the file is not for finds nothing.
            let stranger = Identity::generate().unwrap();
            assert!(parsed.stanzas.iter().all(|s| stranger.unwrap(s).is_none()));
        }
    }

    proptest! {
        // Ported: wrap_and_unwrap (secret keys from 0 to 32 given bytes,
        // the rest zero).
        #[test]
        fn wrap_and_unwrap(sk_bytes in proptest::collection::vec(any::<u8>(), ..=32)) {
            let identity = identity_from(&sk_bytes);
            let file_key = FileKey::generate().unwrap();
            let stanza = identity.to_public().wrap(&file_key).unwrap();
            prop_assert_eq!(stanza.args.len(), 1);
            let back = identity.unwrap(&stanza).unwrap().unwrap();
            prop_assert_eq!(back.expose(), file_key.expose());
        }

        #[test]
        fn random_key_strings_never_panic(s in "\\PC{0,100}") {
            let _ = Identity::parse(&s);
            let _ = Recipient::parse(&s);
        }
    }
}
