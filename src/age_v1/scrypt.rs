//! The "scrypt" passphrase recipient (C2SP age, "The scrypt recipient
//! type").
//!
//! Derived from age 0.12.1, src/native/scrypt.rs, src/primitives.rs
//! (`scrypt`) and src/util.rs (`base64_arg`, `decimal_digit_arg`)
//! (https://github.com/str4d/rage), used under the MIT license:
//! Copyright (c) 2019 Jack Grigg (LICENSES/age-MIT.txt).
//!
//! The stanza is `-> scrypt <base64 salt> <log2 N>` with the wrapped file
//! key as its body. The wrapping key is scrypt(passphrase,
//! "age-encryption.org/v1/scrypt" || salt, N, r = 8, p = 1, 32 bytes).
//!
//! Changes for Crystal: no timed work factor; Crystal's policy passes in
//! the factor to write and the largest to accept (crypto.rs, decision
//! 0027). The passphrase is bytes (age takes a `&str` and uses its bytes).
//! No recipient labels: an scrypt stanza being the only stanza is checked
//! where the header is built and read (`super::encrypt`, `super::decrypt`).
//! Derived keys are zeroizing; scrypt's own working memory is freed
//! through Crystal's zeroizing allocator.

use zeroize::Zeroizing;

use super::header::Stanza;
use super::primitives::{TAG_LEN, aead_decrypt, aead_encrypt};
use super::{Error, FILE_KEY_BYTES, FileKey, base64};

pub(super) const SCRYPT_RECIPIENT_TAG: &str = "scrypt";
const SCRYPT_SALT_LABEL: &[u8] = b"age-encryption.org/v1/scrypt";
const SALT_LEN: usize = 16;
const ENCRYPTED_FILE_KEY_BYTES: usize = FILE_KEY_BYTES + TAG_LEN;

/// scrypt with r = 8 and p = 1 to 32 bytes, over the labelled salt. None
/// for a work factor scrypt refuses (0, or 64 and up).
fn wrapping_key(
    salt: &[u8; SALT_LEN],
    log_n: u8,
    passphrase: &[u8],
) -> Option<Zeroizing<[u8; 32]>> {
    let mut inner_salt = [0; SCRYPT_SALT_LABEL.len() + SALT_LEN];
    inner_salt[..SCRYPT_SALT_LABEL.len()].copy_from_slice(SCRYPT_SALT_LABEL);
    inner_salt[SCRYPT_SALT_LABEL.len()..].copy_from_slice(salt);
    let params = ::scrypt::Params::new(log_n, 8, 1, 32).ok()?;
    let mut key = Zeroizing::new([0; 32]);
    ::scrypt::scrypt(passphrase, &inner_salt, &params, &mut *key).ok()?;
    Some(key)
}

/// age's `decimal_digit_arg`: ASCII digits only, no leading zero (so not
/// "0" either), and within a u8.
fn decimal_digit_arg(arg: &str) -> Option<u8> {
    let digits = !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit());
    if !digits || arg.starts_with('0') {
        return None;
    }
    arg.parse().ok()
}

/// age's `base64_arg`: canonical unpadded base64 of exactly `N` bytes.
fn base64_arg<const N: usize>(arg: &str) -> Option<[u8; N]> {
    base64::decode(arg.as_bytes())?.try_into().ok()
}

/// Wraps `file_key` to `passphrase` at work factor `log_n`, with a fresh
/// random salt. The caller makes it the file's only stanza.
pub(super) fn wrap(file_key: &FileKey, passphrase: &[u8], log_n: u8) -> Result<Stanza, Error> {
    // age's `set_work_factor` asserts 0 < log_n < 64 (scrypt itself would
    // take 0).
    if !(1..64).contains(&log_n) {
        return Err(Error::ExcessiveWork);
    }
    let mut salt = [0; SALT_LEN];
    crate::crypto::fill_random(&mut salt).map_err(|_| Error::Random)?;
    let key = wrapping_key(&salt, log_n, passphrase).ok_or(Error::ExcessiveWork)?;
    // Cannot fail for 16 bytes (ChaCha20's limit is 256 GiB).
    let body = aead_encrypt(&key, file_key.expose()).ok_or(Error::Random)?;
    Ok(Stanza {
        tag: SCRYPT_RECIPIENT_TAG.to_owned(),
        args: vec![base64::encode(&salt), log_n.to_string()],
        body,
    })
}

/// Tries to unwrap `stanza` with `passphrase`, as age's `unwrap_stanza`:
/// None for another stanza type; for a scrypt stanza, the file key or why
/// not. The stanza's form is checked first, then the work factor against
/// `max_log_n`, and only then is any scrypt work done.
pub(super) fn unwrap(
    stanza: &Stanza,
    passphrase: &[u8],
    max_log_n: u8,
) -> Option<Result<FileKey, Error>> {
    if stanza.tag != SCRYPT_RECIPIENT_TAG {
        return None;
    }
    Some(unwrap_scrypt(stanza, passphrase, max_log_n))
}

fn unwrap_scrypt(stanza: &Stanza, passphrase: &[u8], max_log_n: u8) -> Result<FileKey, Error> {
    // Enforce valid and canonical stanza format (c2sp.org/age#scrypt-recipient-stanza).
    let [salt, log_n] = &stanza.args[..] else {
        return Err(Error::InvalidHeader);
    };
    let (Some(salt), Some(log_n)) = (base64_arg::<SALT_LEN>(salt), decimal_digit_arg(log_n)) else {
        return Err(Error::InvalidHeader);
    };
    if stanza.body.len() != ENCRYPTED_FILE_KEY_BYTES {
        return Err(Error::InvalidHeader);
    }
    if log_n > max_log_n {
        return Err(Error::ExcessiveWork);
    }
    let key = wrapping_key(&salt, log_n, passphrase).ok_or(Error::ExcessiveWork)?;
    // From age: "This AEAD is not robust, so an attacker could craft a
    // message that decrypts under two different keys (meaning two different
    // passphrases) and then use an error side-channel in an online
    // decryption oracle to learn if either key is correct. This is deemed
    // acceptable because the use case (an online decryption oracle) is not
    // recommended, and the security loss is only one bit."
    let file_key =
        aead_decrypt(&key, FILE_KEY_BYTES, &stanza.body).ok_or(Error::DecryptionFailed)?;
    FileKey::from_slice(&file_key).ok_or(Error::DecryptionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::age_v1::header;
    use proptest::prelude::*;

    /// Unit tests run scrypt at log N 10 (1 MiB), as crypto.rs's tests do.
    const LOG_N: u8 = 10;

    fn new_key() -> FileKey {
        FileKey::generate().unwrap()
    }

    #[test]
    fn wrap_and_unwrap() {
        let file_key = new_key();
        let stanza = wrap(&file_key, b"correct horse", LOG_N).unwrap();
        assert_eq!(stanza.tag, "scrypt");
        assert_eq!(stanza.args[0].len(), 22, "16 bytes of salt");
        assert_eq!(stanza.args[1], "10");
        assert_eq!(stanza.body.len(), 32);
        let back = unwrap(&stanza, b"correct horse", LOG_N).unwrap().unwrap();
        assert_eq!(back.expose(), file_key.expose());
        // A fresh salt every time.
        assert_ne!(
            wrap(&file_key, b"x", LOG_N).unwrap().args[0],
            wrap(&file_key, b"x", LOG_N).unwrap().args[0]
        );
    }

    #[test]
    fn wrong_passphrase_fails() {
        let stanza = wrap(&new_key(), b"correct horse", LOG_N).unwrap();
        assert_eq!(
            unwrap(&stanza, b"correct hors", LOG_N).unwrap().err(),
            Some(Error::DecryptionFailed)
        );
    }

    #[test]
    fn other_stanzas_are_not_ours() {
        let mut stanza = wrap(&new_key(), b"p", LOG_N).unwrap();
        stanza.tag = "X25519".into();
        assert!(unwrap(&stanza, b"p", LOG_N).is_none());
        // Tags are case-sensitive.
        stanza.tag = "Scrypt".into();
        assert!(unwrap(&stanza, b"p", LOG_N).is_none());
    }

    #[test]
    fn work_factor_bound_is_checked_first() {
        let mut stanza = wrap(&new_key(), b"p", LOG_N).unwrap();
        assert_eq!(
            unwrap(&stanza, b"p", LOG_N - 1).unwrap().err(),
            Some(Error::ExcessiveWork)
        );
        // 2^60 would take forever; refused at once.
        stanza.args[1] = "60".into();
        assert_eq!(
            unwrap(&stanza, b"p", 22).unwrap().err(),
            Some(Error::ExcessiveWork)
        );
        // Allowed by the caller but refused by scrypt itself.
        stanza.args[1] = "64".into();
        assert_eq!(
            unwrap(&stanza, b"p", 255).unwrap().err(),
            Some(Error::ExcessiveWork)
        );
        assert_eq!(wrap(&new_key(), b"p", 0).err(), Some(Error::ExcessiveWork));
        assert_eq!(wrap(&new_key(), b"p", 64).err(), Some(Error::ExcessiveWork));
    }

    // c2sp.org/age#scrypt-recipient-stanza: exactly two arguments, a
    // canonical 16-byte salt, a decimal factor without leading zeros, and a
    // 32-byte body. Each is a header error, before any scrypt work.
    #[test]
    fn malformed_stanzas_are_header_errors() {
        let good = wrap(&new_key(), b"p", LOG_N).unwrap();
        let with_args = |args: &[&str]| Stanza {
            args: args.iter().map(|&a| a.into()).collect(),
            ..good.clone()
        };
        let salt = good.args[0].as_str();
        let short_salt = base64::encode(&[0; 15]);
        let long_salt = base64::encode(&[0; 17]);
        for stanza in [
            with_args(&[salt]),
            with_args(&[salt, "10", "x"]),
            with_args(&[]),
            with_args(&[&short_salt, "10"]),
            with_args(&[&long_salt, "10"]),
            with_args(&[&format!("{salt}="), "10"]),
            with_args(&["AAAAAAAAAAAAAAAAAAAAAB", "10"]),
            with_args(&[salt, "010"]),
            with_args(&[salt, "0"]),
            with_args(&[salt, "+10"]),
            with_args(&[salt, "1e1"]),
            with_args(&[salt, "256"]),
            with_args(&[salt, "١٠"]),
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
                unwrap(&stanza, b"p", 255).unwrap().err(),
                Some(Error::InvalidHeader),
                "{:?}",
                stanza.args
            );
        }
    }

    // A passphrase file from the age crate (0.11.5, whose scrypt code
    // 0.12.1 keeps unchanged; tests/corpus/age-rage/scrypt-utf8.age, at
    // log N 10) opens here, and the file key it yields verifies the header
    // MAC.
    #[test]
    fn opens_age_crate_scrypt_files() {
        let file = crate::age_v1::rage::file("scrypt-utf8.age");
        let (parsed, _) = header::parse(&file).unwrap();
        assert_eq!(parsed.stanzas.len(), 1);
        let key = unwrap(&parsed.stanzas[0], "pässwörd".as_bytes(), LOG_N)
            .unwrap()
            .unwrap();
        assert!(parsed.verify_mac(key.expose()));
        assert_eq!(
            unwrap(&parsed.stanzas[0], b"wrong", LOG_N).unwrap().err(),
            Some(Error::DecryptionFailed)
        );
    }

    proptest! {
        // log N 1: scrypt costs microseconds, so this runs at any case count.
        #[test]
        fn round_trip(passphrase in proptest::collection::vec(any::<u8>(), 1..64)) {
            let file_key = new_key();
            let stanza = wrap(&file_key, &passphrase, 1).unwrap();
            let back = unwrap(&stanza, &passphrase, 1).unwrap().unwrap();
            prop_assert_eq!(back.expose(), file_key.expose());
        }
    }
}
