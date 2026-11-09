//! The age v1 payload: STREAM (C2SP age, "Payload").
//!
//! Derived from age 0.12.1, src/primitives/stream.rs and src/keys.rs
//! (https://github.com/str4d/rage), used under the MIT license:
//! Copyright (c) 2019 Jack Grigg (LICENSES/age-MIT.txt).
//!
//! The payload key is HKDF-SHA-256(salt = the 16-byte payload nonce,
//! label "payload", file key). The plaintext is split into 64 KiB chunks,
//! each encrypted with ChaCha20-Poly1305 under a 12-byte nonce: an 11-byte
//! big-endian chunk counter from 0, then 0x01 for the last chunk and 0x00
//! for every other. Only the last chunk may be shorter than 64 KiB, and it
//! may be empty only when the whole plaintext is empty.
//!
//! Changes for Crystal: whole buffers instead of age's `StreamWriter` and
//! `StreamReader` (no seeking, no async); each chunk is encrypted and
//! decrypted in place in the output buffer, which is zeroizing and sized
//! once. The rules are age's: a full-size final chunk is decrypted only as
//! the last chunk, so a missing last flag (truncation), data after the
//! last chunk, swapped chunks, and an empty last chunk after others all
//! fail.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, Tag};
use zeroize::Zeroizing;

use super::FILE_KEY_BYTES;
use super::primitives::{TAG_LEN, hkdf};

/// Plaintext bytes per chunk.
pub(super) const CHUNK_SIZE: usize = 64 << 10;
const ENCRYPTED_CHUNK_SIZE: usize = CHUNK_SIZE + TAG_LEN;
/// The random nonce at the start of the payload, the payload key's salt.
pub(super) const PAYLOAD_NONCE_LEN: usize = 16;
/// age's HKDF label for the payload key.
const PAYLOAD_KEY_LABEL: &[u8] = b"payload";

/// The payload key: HKDF-SHA-256(nonce, "payload", file key), as age's
/// `v1_payload_key` derives it.
pub(super) fn payload_key(
    file_key: &[u8; FILE_KEY_BYTES],
    nonce: &[u8; PAYLOAD_NONCE_LEN],
) -> Zeroizing<[u8; 32]> {
    hkdf(nonce, PAYLOAD_KEY_LABEL, file_key)
}

/// The nonce of chunk `counter`: 11 bytes of big-endian counter (a u64
/// fills the low 8; a 32 MiB payload has 512 chunks), then the last flag.
fn chunk_nonce(counter: u64, last: bool) -> Nonce {
    let mut nonce = Nonce::default();
    nonce[3..11].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    nonce
}

/// The number of chunks for a plaintext of `len` bytes: at least one, as
/// an empty plaintext is one empty last chunk.
fn chunk_count(len: usize) -> usize {
    len.div_ceil(CHUNK_SIZE).max(1)
}

/// Encrypts `plaintext` under the payload key `key` (without the payload
/// nonce, which the caller writes first). None only if the length
/// overflows, which no buffer in memory can.
pub(super) fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Option<Vec<u8>> {
    let chunks = chunk_count(plaintext.len());
    let len = plaintext.len().checked_add(chunks.checked_mul(TAG_LEN)?)?;
    let cipher = ChaCha20Poly1305::new(key.into());
    // Each chunk's plaintext is copied in and encrypted where it lies, so
    // no plaintext is left once the loop ends; if it stops early, the
    // buffer is wiped. Exact capacity: no reallocation.
    let mut out = Zeroizing::new(Vec::with_capacity(len));
    for (i, chunk) in plaintext_chunks(plaintext).enumerate() {
        let last = i + 1 == chunks;
        let start = out.len();
        out.extend_from_slice(chunk);
        let nonce = chunk_nonce(u64::try_from(i).ok()?, last);
        let tag = cipher
            .encrypt_in_place_detached(&nonce, b"", &mut out[start..])
            .ok()?;
        out.extend_from_slice(&tag);
    }
    // Ciphertext only now: move it out without a copy.
    Some(std::mem::take(&mut *out))
}

/// `plaintext` in 64 KiB chunks; one empty chunk for an empty plaintext.
fn plaintext_chunks(plaintext: &[u8]) -> impl Iterator<Item = &[u8]> {
    let empty: &[u8] = &[];
    plaintext
        .chunks(CHUNK_SIZE)
        .chain(plaintext.is_empty().then_some(empty))
}

/// Decrypts a STREAM ciphertext (after the payload nonce) under the
/// payload key `key`. None for an empty ciphertext (no last chunk), a
/// final piece shorter than a tag, an empty last chunk after others, and
/// any chunk that fails to authenticate in its position with its flag.
pub(super) fn decrypt(key: &[u8; 32], ciphertext: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if ciphertext.is_empty() {
        return None;
    }
    let chunks = ciphertext.len().div_ceil(ENCRYPTED_CHUNK_SIZE);
    let last_len = ciphertext.len() - (chunks - 1) * ENCRYPTED_CHUNK_SIZE;
    if last_len < TAG_LEN || (last_len == TAG_LEN && chunks > 1) {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(key.into());
    // Exactly the plaintext's size: decrypted in place, never reallocated.
    let mut out = Zeroizing::new(Vec::with_capacity(ciphertext.len() - chunks * TAG_LEN));
    for (i, chunk) in ciphertext.chunks(ENCRYPTED_CHUNK_SIZE).enumerate() {
        // Every chunk holds at least a tag: full ones, and the last was
        // checked above.
        let (body, tag) = chunk.split_at(chunk.len() - TAG_LEN);
        let start = out.len();
        out.extend_from_slice(body);
        let nonce = chunk_nonce(u64::try_from(i).ok()?, i + 1 == chunks);
        // The tag is checked before anything is decrypted.
        cipher
            .decrypt_in_place_detached(&nonce, b"", &mut out[start..], Tag::from_slice(tag))
            .ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::OnceLock;

    const KEY: [u8; 32] = [7; 32];

    /// One chunk encrypted on its own, with any counter and flag, to build
    /// streams `encrypt` never makes.
    fn chunk(counter: u64, last: bool, data: &[u8]) -> Vec<u8> {
        let mut buf = data.to_vec();
        let tag = ChaCha20Poly1305::new(&KEY.into())
            .encrypt_in_place_detached(&chunk_nonce(counter, last), b"", &mut buf)
            .unwrap();
        buf.extend_from_slice(&tag);
        buf
    }

    // Ported from age 0.12.1, src/primitives/stream.rs: stream_round_trip
    // and its _empty, _short, _chunk and _long cases, plus the chunk
    // boundaries.
    #[test]
    fn stream_round_trip() {
        for len in [
            0,
            1,
            1024,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            100 * 1024,
            2 * CHUNK_SIZE,
            3 * CHUNK_SIZE + 5,
        ] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let encrypted = encrypt(&KEY, &data).unwrap();
            assert_eq!(encrypted.len(), len + chunk_count(len) * TAG_LEN, "{len}");
            assert_eq!(*decrypt(&KEY, &encrypted).unwrap(), data, "{len}");
        }
    }

    // The 32 MiB maximum Crystal encrypts: 512 full chunks, the last
    // flagged. About 10 s in a debug build, so release builds run it.
    #[test]
    #[cfg_attr(debug_assertions, ignore = "slow in debug builds; runs in release")]
    fn stream_round_trip_32_mib() {
        let data = vec![0x5a; 32 << 20];
        let encrypted = encrypt(&KEY, &data).unwrap();
        assert_eq!(encrypted.len(), data.len() + 512 * TAG_LEN);
        assert!(*decrypt(&KEY, &encrypted).unwrap() == data);
    }

    // Ported: chunk_round_trip and last_chunk_round_trip. A chunk opens
    // only with the flag it was sealed with.
    #[test]
    fn chunk_flags() {
        let data = vec![42; CHUNK_SIZE];
        let as_last = chunk(0, true, &data);
        assert_eq!(*decrypt(&KEY, &as_last).unwrap(), data);
        assert_eq!(as_last, encrypt(&KEY, &data).unwrap());
        // The same chunk sealed as not last: the stream has no end.
        assert!(decrypt(&KEY, &chunk(0, false, &data)).is_none());
        assert!(decrypt(&KEY, &chunk(0, false, b"short")).is_none());
    }

    // Ported: stream_fails_to_decrypt_truncated_file (the writer is never
    // finished, so no chunk carries the last flag), and every cut at a
    // chunk boundary.
    #[test]
    fn truncation_fails() {
        let data = vec![42; 2 * CHUNK_SIZE];
        let unfinished = [
            chunk(0, false, &data[..CHUNK_SIZE]),
            chunk(1, false, &data[CHUNK_SIZE..]),
        ]
        .concat();
        assert!(decrypt(&KEY, &unfinished).is_none());

        let full = encrypt(&KEY, &[42; 2 * CHUNK_SIZE + 3]).unwrap();
        for cut in [
            0,
            1,
            TAG_LEN,
            ENCRYPTED_CHUNK_SIZE,
            2 * ENCRYPTED_CHUNK_SIZE,
            full.len() - 1,
        ] {
            assert!(decrypt(&KEY, &full[..cut]).is_none(), "{cut}");
        }
    }

    #[test]
    fn trailing_data_fails() {
        let one = encrypt(&KEY, b"hello").unwrap();
        assert!(decrypt(&KEY, &[&one[..], &[0]].concat()).is_none());
        // A whole valid-looking chunk after the last one.
        let after = chunk(1, true, b"more");
        assert!(decrypt(&KEY, &[&one[..], &after[..]].concat()).is_none());
        // A full last chunk with another chunk after it.
        let full = encrypt(&KEY, &[1; CHUNK_SIZE]).unwrap();
        assert!(decrypt(&KEY, &[&full[..], &after[..]].concat()).is_none());
    }

    #[test]
    fn reordered_chunks_fail() {
        let data: Vec<u8> = (0..3 * CHUNK_SIZE)
            .map(|i| (i / CHUNK_SIZE) as u8)
            .collect();
        let encrypted = encrypt(&KEY, &[&data[..], b"end"].concat()).unwrap();
        let mut swapped = encrypted.clone();
        let (a, b) = swapped.split_at_mut(ENCRYPTED_CHUNK_SIZE);
        a.swap_with_slice(&mut b[..ENCRYPTED_CHUNK_SIZE]);
        assert!(decrypt(&KEY, &swapped).is_none());
    }

    // age refuses an empty last chunk after a full one
    // (err-stream-last-chunk-empty); a lone empty chunk is the empty file.
    #[test]
    fn empty_last_chunk_only_for_empty_plaintext() {
        let lone = chunk(0, true, b"");
        assert_eq!(lone, encrypt(&KEY, b"").unwrap());
        assert!(decrypt(&KEY, &lone).unwrap().is_empty());
        let after_full = [chunk(0, false, &[1; CHUNK_SIZE]), chunk(1, true, b"")].concat();
        assert!(decrypt(&KEY, &after_full).is_none());
    }

    #[test]
    fn nonce_layout() {
        assert_eq!(chunk_nonce(0, false)[..], [0; 12]);
        assert_eq!(
            chunk_nonce(1, true)[..],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1]
        );
        assert_eq!(
            chunk_nonce(0x0102_0304_0506_0708, false)[..],
            [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 0]
        );
    }

    // HKDF-SHA-256(salt nonce, info "payload", file key), recomputed with
    // the hkdf crate directly.
    #[test]
    fn payload_key_construction() {
        let (file_key, nonce) = ([3; 16], [4; 16]);
        let mut okm = [0; 32];
        ::hkdf::Hkdf::<sha2::Sha256>::new(Some(&nonce), &file_key)
            .expand(b"payload", &mut okm)
            .unwrap();
        assert_eq!(*payload_key(&file_key, &nonce), okm);
    }

    /// Two full chunks and a short one, encrypted once for the property
    /// tests below.
    fn sample() -> &'static [u8] {
        static SAMPLE: OnceLock<Vec<u8>> = OnceLock::new();
        SAMPLE.get_or_init(|| encrypt(&KEY, &[9; 2 * CHUNK_SIZE + 10]).unwrap())
    }

    proptest! {
        #[test]
        fn round_trip(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let encrypted = encrypt(&KEY, &data).unwrap();
            prop_assert_eq!(&*decrypt(&KEY, &encrypted).unwrap(), &data[..]);
        }

        #[test]
        fn flipped_bit_fails(bit in any::<prop::sample::Index>()) {
            let mut bytes = sample().to_vec();
            let bit = bit.index(bytes.len() * 8);
            bytes[bit / 8] ^= 1 << (bit % 8);
            prop_assert!(decrypt(&KEY, &bytes).is_none());
        }

        #[test]
        fn truncation_or_extension_fails(
            cut in any::<prop::sample::Index>(),
            extra in proptest::collection::vec(any::<u8>(), 1..100),
        ) {
            let bytes = sample();
            prop_assert!(decrypt(&KEY, &bytes[..cut.index(bytes.len())]).is_none());
            prop_assert!(decrypt(&KEY, &[bytes, &extra[..]].concat()).is_none());
        }

        #[test]
        fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            prop_assert!(decrypt(&KEY, &bytes).is_none());
        }
    }
}
