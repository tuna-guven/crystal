//! Base64 as age requires it: the standard alphabet of RFC 4648 section 4,
//! no padding, canonical only. Used for stanza arguments (the X25519 share,
//! the scrypt salt), stanza bodies and the header MAC; all public bytes.
//!
//! Written for Crystal, not ported: age uses the base64 crate's
//! `BASE64_STANDARD_NO_PAD`, whose decoder rejects padding and non-zero
//! trailing bits. These are the same rules (C2SP age: "the decoder MUST
//! reject non-canonical encodings and encodings ending with padding").

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The length of `encode` for `n` bytes: 4 characters per 3 bytes, and 2
/// or 3 for a final 1 or 2 bytes. No overflow for any slice length.
pub(super) fn encoded_len(n: usize) -> usize {
    n / 3 * 4 + [0, 2, 3][n % 3]
}

/// `bytes` as unpadded base64.
pub(super) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(encoded_len(bytes.len()));
    for chunk in bytes.chunks(3) {
        // The chunk as the top 24 bits' worth of a big-endian number;
        // missing bytes are zero.
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        // 1 byte: 2 characters; 2 bytes: 3; 3 bytes: 4.
        for i in 0..=chunk.len() {
            let index = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

/// The 6-bit value of one alphabet character; anything else (padding,
/// whitespace, the URL-safe `-` and `_`) is None.
fn value(c: u8) -> Option<u32> {
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(v))
}

/// Decodes unpadded, canonical base64. None for any character outside the
/// alphabet (so also for `=` and whitespace), for a length that no byte
/// count encodes to (4k + 1), and for a non-canonical final character:
/// the bits below the last whole byte must be zero, so every byte string
/// has exactly one accepted encoding.
pub(super) fn decode(text: &[u8]) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 2);
    for chunk in text.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        // 2 characters hold 1 byte, 3 hold 2, 4 hold 3. The last
        // character's leftover bits (4 for 1 byte, 2 for 2) must be zero.
        let whole = chunk.len() - 1;
        let leftover = n & (0x00ff_ffff >> (8 * whole));
        if leftover != 0 {
            return None;
        }
        out.extend_from_slice(&bytes[1..=whole]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // RFC 4648 section 10, without the padding.
    const RFC_4648: [(&str, &str); 7] = [
        ("", ""),
        ("f", "Zg"),
        ("fo", "Zm8"),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg"),
        ("fooba", "Zm9vYmE"),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn rfc_4648_vectors() {
        for (plain, encoded) in RFC_4648 {
            assert_eq!(encode(plain.as_bytes()), encoded);
            assert_eq!(encoded_len(plain.len()), encoded.len());
            assert_eq!(decode(encoded.as_bytes()).unwrap(), plain.as_bytes());
        }
    }

    // Every character once, values 0 to 63 in order: 64 characters are 48
    // bytes, and they must encode back to the alphabet.
    #[test]
    fn whole_alphabet() {
        let decoded = decode(ALPHABET).unwrap();
        assert_eq!(decoded.len(), 48);
        assert_eq!(encode(&decoded).as_bytes(), ALPHABET);
    }

    #[test]
    fn padding_is_rejected() {
        for bad in ["Zg==", "Zm8=", "Zg=", "Zm9v====", "="] {
            assert_eq!(decode(bad.as_bytes()), None, "{bad}");
        }
    }

    // The bits below the last whole byte must be zero: "Zg" is the only
    // spelling of "f", and "Zm8" the only one of "fo".
    #[test]
    fn non_canonical_is_rejected() {
        for bad in ["Zh", "Zv", "Zm9", "Zm/", "Zm9vYh", "Zm9vYmF"] {
            assert_eq!(decode(bad.as_bytes()), None, "{bad}");
        }
    }

    #[test]
    fn foreign_characters_and_lengths_are_rejected() {
        for bad in [
            "Z", "Zm9vY", "Zm 9v", "Zm9v\n", " Zm9v", "Zm-v", "Zm_v", "Zm9v\0", "Zm.v",
        ] {
            assert_eq!(decode(bad.as_bytes()), None, "{bad:?}");
        }
        assert_eq!(decode("Zm\u{e9}v".as_bytes()), None);
    }

    proptest! {
        #[test]
        fn round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let encoded = encode(&bytes);
            prop_assert_eq!(encoded.len(), encoded_len(bytes.len()));
            prop_assert_eq!(decode(encoded.as_bytes()), Some(bytes));
        }

        // Canonical: whatever decodes encodes back to exactly the input,
        // and random input never panics.
        #[test]
        fn decode_is_canonical(text in proptest::collection::vec(any::<u8>(), 0..80)) {
            if let Some(bytes) = decode(&text) {
                prop_assert_eq!(encode(&bytes).into_bytes(), text);
            }
        }

        #[test]
        fn alphabet_text_is_canonical(text in "[A-Za-z0-9+/]{0,80}") {
            if let Some(bytes) = decode(text.as_bytes()) {
                prop_assert_eq!(encode(&bytes), text);
            }
        }
    }
}
