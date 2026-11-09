//! Derived from age-core 0.12.0, src/primitives.rs
//! (https://github.com/str4d/rage), used under the MIT license:
//! Copyright (c) 2019 Jack Grigg (LICENSES/age-MIT.txt).
//!
//! Changes for Crystal: the AEAD works in place in zeroizing buffers;
//! decryption returns its plaintext (a file key) in a `Zeroizing` buffer
//! and the HKDF output is `Zeroizing`; failures are `None` instead of
//! `aead::Error`; encryption returns `None` instead of panicking.
//!
//! The derived keys are wiped, and so is the AEAD's own copy of its key
//! (chacha20poly1305 zeroizes on drop). HKDF's internal HMAC state lives
//! on the stack and is not wiped by the hkdf crate, as in age.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

/// The Poly1305 tag ChaCha20-Poly1305 appends.
pub(super) const TAG_LEN: usize = 16;

/// `encrypt[key](plaintext)`: ChaCha20-Poly1305 (RFC 8439) with a zero
/// nonce and no associated data. Safe only because every key is used
/// once (a fresh stanza key per file). None only past ChaCha20's 256 GiB
/// block-counter limit, which a 16-byte file key never reaches.
pub(super) fn aead_encrypt(key: &[u8; 32], plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    // Exact capacity: the tag fits without reallocating, so the plaintext
    // copied in is overwritten in place, and wiped if encryption fails.
    let mut buffer = Zeroizing::new(Vec::with_capacity(plaintext.len() + TAG_LEN));
    buffer.extend_from_slice(plaintext);
    cipher
        .encrypt_in_place(&Nonce::default(), b"", &mut *buffer)
        .ok()?;
    // Ciphertext, no longer secret: move it out without a copy.
    Some(std::mem::take(&mut *buffer))
}

/// `decrypt[key](ciphertext)` of a message of exactly `size` bytes.
///
/// From age: the size is checked first "to mitigate multi-key attacks,
/// where a ciphertext can be crafted that decrypts successfully under
/// multiple keys. Short ciphertexts can only target two keys, which has
/// limited impact." None for a wrong length, key or tag.
pub(super) fn aead_decrypt(
    key: &[u8; 32],
    size: usize,
    ciphertext: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    if Some(ciphertext.len()) != size.checked_add(TAG_LEN) {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut buffer = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    buffer.extend_from_slice(ciphertext);
    // Checks the tag before decrypting; on success it truncates the tag
    // off. Zeroizing<Vec> wipes the whole capacity on drop.
    cipher
        .decrypt_in_place(&Nonce::default(), b"", &mut *buffer)
        .ok()?;
    Some(buffer)
}

/// `HKDF[salt, label](ikm, 32)`: HKDF from RFC 5869 with SHA-256.
pub(super) fn hkdf(salt: &[u8], label: &[u8], ikm: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut okm = Zeroizing::new([0; 32]);
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(label, &mut *okm)
        // HKDF-SHA-256 refuses only outputs over 255 * 32 bytes.
        .expect("32 bytes is a valid HKDF-SHA-256 output length");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;
    use chacha20poly1305::aead::Aead;
    use proptest::prelude::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        s.chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    // Ported from age-core 0.12.0, src/primitives.rs.
    #[test]
    fn aead_round_trip() {
        let key = [14; 32];
        let plaintext = b"12345678";
        let encrypted = aead_encrypt(&key, plaintext).unwrap();
        let decrypted = aead_decrypt(&key, plaintext.len(), &encrypted).unwrap();
        assert_eq!(&*decrypted, plaintext);
    }

    #[test]
    fn aead_refuses_wrong_size_key_and_tag() {
        let key = [14; 32];
        let encrypted = aead_encrypt(&key, &[7; 16]).unwrap();
        assert_eq!(encrypted.len(), 32);
        assert!(aead_decrypt(&key, 15, &encrypted).is_none());
        assert!(aead_decrypt(&key, 17, &encrypted).is_none());
        assert!(aead_decrypt(&key, usize::MAX, &encrypted).is_none());
        assert!(aead_decrypt(&key, 15, &encrypted[..31]).is_none());
        assert!(aead_decrypt(&[15; 32], 16, &encrypted).is_none());
        for bit in 0..encrypted.len() * 8 {
            let mut bad = encrypted.clone();
            bad[bit / 8] ^= 1 << (bit % 8);
            assert!(aead_decrypt(&key, 16, &bad).is_none(), "bit {bit}");
        }
        assert!(aead_decrypt(&key, 0, &[0; 16]).is_none());
    }

    // RFC 5869 appendix A, test cases 1 and 3 (SHA-256), first 32 bytes of
    // the OKM: HKDF's first output block does not depend on the length.
    // Case 3's empty salt is the same as an absent one (a zero key).
    #[test]
    fn rfc5869_vectors() {
        let ikm = hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        let okm = hkdf(
            &hex("000102030405060708090a0b0c"),
            &hex("f0f1f2f3f4f5f6f7f8f9"),
            &ikm,
        );
        assert_eq!(
            okm[..],
            hex("3cb25f25faacd57a90434f64d0362f2a 2d2d0a90cf1a5a4c5db02d56ecc4c5bf")[..]
        );
        let okm = hkdf(b"", b"", &ikm);
        assert_eq!(
            okm[..],
            hex("8da4e775a563c18f715f802a063c5a31 b8a11f5c5ee1879ec3454e5f3c738d2d")[..]
        );
    }

    proptest! {
        // The in-place AEAD gives exactly what age-core's allocating
        // `Aead::encrypt` with a zero nonce gives.
        #[test]
        fn same_as_age_core(
            key in any::<[u8; 32]>(),
            plaintext in proptest::collection::vec(any::<u8>(), 0..100),
        ) {
            let theirs = ChaCha20Poly1305::new(&key.into())
                .encrypt(&[0; 12].into(), &plaintext[..])
                .unwrap();
            let ours = aead_encrypt(&key, &plaintext).unwrap();
            prop_assert_eq!(&ours, &theirs);
            let back = aead_decrypt(&key, plaintext.len(), &ours).unwrap();
            prop_assert_eq!(&*back, &plaintext[..]);
        }

        #[test]
        fn random_ciphertext_is_refused(
            key in any::<[u8; 32]>(),
            ciphertext in proptest::collection::vec(any::<u8>(), 0..64),
            size in 0usize..64,
        ) {
            // A forged tag passes with probability 2^-128.
            prop_assert!(aead_decrypt(&key, size, &ciphertext).is_none());
        }
    }
}
