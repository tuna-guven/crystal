//! Bech32 (BIP 173) for age's key strings: `age1…` recipients and
//! `AGE-SECRET-KEY-1…` identities.
//!
//! Written for Crystal from BIP 173, not ported: age does not implement
//! Bech32 itself. age 0.12.1 calls the bech32 0.11.1 crate
//! (`CheckedHrpstring::new::<Bech32Long>` in age-core 0.12.0: the Bech32
//! checksum without the length limit); age 0.11.5 called bech32 0.9.1.
//! This module accepts:
//! - the separator is the last `1`; the human-readable part (HRP) is 1 to
//!   83 characters from ASCII 33 to 126;
//! - the whole string is one case, upper or lower; the checksum is
//!   computed over the lowercase HRP;
//! - the checksum is Bech32 (constant 1); Bech32m strings are refused;
//! - the 5-bit groups regroup into bytes with fewer than 5 leftover bits,
//!   all zero (BIP 173's `convertbits` without padding);
//! - no overall 90-character limit (BIP 173 has one; age does not apply
//!   it, and age's post-quantum recipients are longer).
//!
//! All of this matches age 0.12.1 except the padding: bech32 0.11.1's
//! byte iterator drops leftover bits unchecked, so age 0.12.1 would also
//! take a key string whose 4 padding bits are not zero. Crystal refuses
//! it, as Go age, age 0.11.5 and BIP 173's reference code do. No age
//! implementation writes such a string, so every real key reads the same.
//!
//! The data may be a secret key, so every buffer holding it is zeroizing
//! and sized once, never reallocated.

use zeroize::Zeroizing;

/// The 32 data characters; a character's index is its 5-bit value.
const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// BIP 173's BCH generator.
const GEN: [u32; 5] = [
    0x3b6a_57b2,
    0x2650_8e6d,
    0x1ea1_19fa,
    0x3d42_33dd,
    0x2a14_62b3,
];

const CHECKSUM_LEN: usize = 6;

/// What the polymod over HRP, data and checksum must be for Bech32.
/// Bech32m (BIP 350) uses 0x2bc830a3, which age refuses.
const BECH32_CONST: u32 = 1;

const MAX_HRP_LEN: usize = 83;

/// BIP 173's checksum state, fed one 5-bit value at a time.
struct Polymod(u32);

impl Polymod {
    /// The state after the expanded HRP: the high bits of each character,
    /// a zero, then the low 5 bits of each. `hrp` is lowercased here, as
    /// the checksum is defined over the lowercase form.
    fn new(hrp: &[u8]) -> Self {
        let mut p = Self(1);
        for &c in hrp {
            p.step(c.to_ascii_lowercase() >> 5);
        }
        p.step(0);
        for &c in hrp {
            p.step(c.to_ascii_lowercase() & 31);
        }
        p
    }

    fn step(&mut self, value: u8) {
        let top = self.0 >> 25;
        self.0 = ((self.0 & 0x01ff_ffff) << 5) ^ u32::from(value);
        for (i, g) in GEN.iter().enumerate() {
            // XOR g in when bit i of `top` is set, without a branch on it:
            // the mask is all ones or all zeros.
            self.0 ^= g & 0u32.wrapping_sub((top >> i) & 1);
        }
    }
}

/// The case seen so far; a string must not mix upper and lower case.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    None,
    Lower,
    Upper,
}

impl Case {
    /// The case after `c`; None if `c` contradicts it.
    fn with(self, c: u8) -> Option<Self> {
        match (self, c) {
            (Self::Upper, b'a'..=b'z') | (Self::Lower, b'A'..=b'Z') => None,
            (_, b'a'..=b'z') => Some(Self::Lower),
            (_, b'A'..=b'Z') => Some(Self::Upper),
            (case, _) => Some(case),
        }
    }
}

/// The 5-bit value of a data character, either case.
fn value(c: u8) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    CHARSET
        .iter()
        .position(|&x| x == lower)
        .and_then(|i| u8::try_from(i).ok())
}

/// `data` as a lowercase Bech32 string with human-readable part `hrp`
/// (a lowercase constant: `age` or `age-secret-key-`). An identity is
/// shown in upper case; the caller uppercases the result in place.
pub(super) fn encode(hrp: &str, data: &[u8]) -> Zeroizing<String> {
    debug_assert!(
        !hrp.is_empty()
            && hrp.len() <= MAX_HRP_LEN
            && hrp
                .bytes()
                .all(|c| (33..=126).contains(&c) && !c.is_ascii_uppercase()),
        "HRP must be a valid lowercase constant"
    );
    let groups = (data.len() * 8).div_ceil(5);
    let mut out = Zeroizing::new(String::with_capacity(hrp.len() + 1 + groups + CHECKSUM_LEN));
    out.push_str(hrp);
    out.push('1');
    let mut checksum = Polymod::new(hrp.as_bytes());
    let mut push = |v: u8, out: &mut String| {
        checksum.step(v);
        out.push(char::from(CHARSET[usize::from(v)]));
    };
    // 8 bits in, 5 bits out; `acc` keeps at most 12 bits.
    let mut acc = 0u32;
    let mut bits = 0;
    for &byte in data {
        acc = ((acc << 8) | u32::from(byte)) & 0xfff;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            push(((acc >> bits) & 31) as u8, &mut out);
        }
    }
    if bits > 0 {
        push(((acc << (5 - bits)) & 31) as u8, &mut out);
    }
    for _ in 0..CHECKSUM_LEN {
        checksum.step(0);
    }
    let sum = checksum.0 ^ BECH32_CONST;
    for i in 0..CHECKSUM_LEN {
        let v = (sum >> (5 * (CHECKSUM_LEN - 1 - i))) & 31;
        out.push(char::from(CHARSET[v as usize]));
    }
    out
}

/// Decodes a Bech32 string: the lowercase HRP and the data bytes. None for
/// anything outside the rules in the module doc. The caller checks the HRP
/// and the length.
pub(super) fn decode(s: &str) -> Option<(String, Zeroizing<Vec<u8>>)> {
    let bytes = s.as_bytes();
    let sep = bytes.iter().rposition(|&b| b == b'1')?;
    let (hrp, rest) = (&bytes[..sep], &bytes[sep + 1..]);
    if hrp.is_empty() || hrp.len() > MAX_HRP_LEN || rest.len() < CHECKSUM_LEN {
        return None;
    }
    let mut case = Case::None;
    for &c in hrp {
        if !(33..=126).contains(&c) {
            return None;
        }
        case = case.with(c)?;
    }
    let mut checksum = Polymod::new(hrp);
    let groups = rest.len() - CHECKSUM_LEN;
    // Exactly the whole bytes in `groups` 5-bit values: no reallocation.
    let mut out = Zeroizing::new(Vec::with_capacity(groups * 5 / 8));
    let mut acc = 0u32;
    let mut bits = 0;
    for (i, &c) in rest.iter().enumerate() {
        case = case.with(c)?;
        let v = value(c)?;
        checksum.step(v);
        if i < groups {
            acc = ((acc << 5) | u32::from(v)) & 0xfff;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
    }
    if checksum.0 != BECH32_CONST {
        return None;
    }
    // A whole 5-bit group left over, or non-zero padding, is not canonical.
    if bits >= 5 || acc & ((1 << bits) - 1) != 0 {
        return None;
    }
    // ASCII 33 to 126, checked above, is valid UTF-8.
    let hrp = String::from_utf8(hrp.to_ascii_lowercase()).ok()?;
    Some((hrp, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The test key pair from age 0.12.1 (src/native/x25519.rs, tests).
    const TEST_SK: &str =
        "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";
    const TEST_PK: &str = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa";

    /// A checksummed string from raw 5-bit values, so tests can build
    /// strings `encode` never makes (bad padding).
    fn with_checksum(hrp: &str, values: &[u8]) -> String {
        let mut p = Polymod::new(hrp.as_bytes());
        let mut s = format!("{hrp}1");
        for &v in values {
            p.step(v);
            s.push(char::from(CHARSET[usize::from(v)]));
        }
        for _ in 0..CHECKSUM_LEN {
            p.step(0);
        }
        let sum = p.0 ^ BECH32_CONST;
        for i in 0..CHECKSUM_LEN {
            s.push(char::from(CHARSET[((sum >> (5 * (5 - i))) & 31) as usize]));
        }
        s
    }

    // BIP 173 "Test vectors", valid Bech32 strings (also in bech32 0.9.1's
    // tests). Each decodes and encodes back to its lowercase form.
    #[test]
    fn bip173_valid() {
        for s in [
            "A12UEL5L",
            "a12uel5l",
            "an83characterlonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1tt5tgs",
            "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw",
            "11qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqc8247j",
            "split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w",
            "?1ezyfcl",
        ] {
            let (hrp, data) = decode(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(*encode(&hrp, &data), s.to_lowercase(), "{s}");
        }
    }

    // BIP 173 invalid strings (and bech32 0.9.1's list), each refused.
    #[test]
    fn bip173_invalid() {
        for s in [
            "\u{20}1nwldj5",      // HRP character out of range
            "\u{7f}1axkwrx",      // HRP character out of range
            "\u{80}1eym55h",      // HRP character out of range
            "abc1\u{2192}axkwrx", // non-ASCII data
            "an84characterslonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1569pvx",
            "pzry9x0s0muk",   // no separator
            "1pzry9x0s0muk",  // empty HRP
            "x1b4n0q5v",      // invalid data character
            "ABC1DEFGOH",     // invalid data character
            "li1dgmt3",       // checksum too short
            "de1lg7wt\u{ff}", // invalid character in checksum
            "A1G7SGD8",       // checksum over the uppercase HRP
            "10a06t8",        // empty HRP
            "1qzzfhee",       // empty HRP
            "an84characterslonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11d6pts4",
            "qyrz8wqd2c9m",
            "1qyrz8wqd2c9m",
            "y1b0jsk6g",
            "lt1igcx5c0",
            "in1muywd",
            "mm1crxm3i",
            "au1s5cgom",
            "M1VUXWEZ",
            "16plkw9",
            "1p2gdwpf",
            "bc1p2",
            "",
            "1",
        ] {
            assert!(decode(s).is_none(), "{s:?}");
        }
    }

    // BIP 350 valid Bech32m strings: age accepts only Bech32.
    #[test]
    fn bech32m_is_refused() {
        for s in [
            "A1LQFN3A",
            "a1lqfn3a",
            "an83characterlonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11sg7hg6",
            "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
            "11llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllludsr8",
            "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
            "?1v759aa",
        ] {
            assert!(decode(s).is_none(), "{s}");
        }
    }

    // bech32 0.9.1's conversion vectors: [0x1f, 0x1c] is 0xff with two
    // zero bits left; [0x1f] (a whole leftover group) and [0x1f, 0x1f]
    // (non-zero padding) are refused.
    #[test]
    fn padding_rules() {
        let (_, data) = decode(&with_checksum("a", &[0x1f, 0x1c])).unwrap();
        assert_eq!(*data, [0xff]);
        assert!(decode(&with_checksum("a", &[0x1f])).is_none());
        assert!(decode(&with_checksum("a", &[0x1f, 0x1f])).is_none());
        assert!(decode(&with_checksum("a", &[0x1f, 0x1d])).is_none());
        assert_eq!(*encode("a", &[0xff]), with_checksum("a", &[0x1f, 0x1c]));
    }

    #[test]
    fn age_test_keys() {
        let (hrp, pk) = decode(TEST_PK).unwrap();
        assert_eq!(hrp, "age");
        assert_eq!(pk.len(), 32);
        assert_eq!(*encode("age", &pk), TEST_PK);

        let (hrp, sk) = decode(TEST_SK).unwrap();
        assert_eq!(hrp, "age-secret-key-");
        assert_eq!(sk.len(), 32);
        let mut again = encode("age-secret-key-", &sk);
        again.make_ascii_uppercase();
        assert_eq!(*again, TEST_SK);
        // Lowercase is the same key; mixed case is refused.
        assert_eq!(decode(&TEST_SK.to_lowercase()).unwrap().1, sk);
        assert!(decode(&TEST_SK.replacen('G', "g", 1)).is_none());
        assert!(decode(&TEST_PK.replacen('t', "T", 1)).is_none());
    }

    // encode sizes its buffer exactly, so it never reallocates.
    #[test]
    fn encode_allocates_once() {
        for n in 0..70 {
            let s = encode("age-secret-key-", &vec![0xa5; n]);
            assert_eq!(s.len(), s.capacity(), "{n}");
        }
    }

    proptest! {
        #[test]
        fn round_trip(data in proptest::collection::vec(any::<u8>(), 0..64)) {
            let s = encode("age", &data);
            let (hrp, back) = decode(&s).unwrap();
            prop_assert_eq!(hrp, "age");
            prop_assert_eq!(&*back, &data[..]);
        }

        // Any one changed byte in a key string is caught: another character
        // fails the checksum (BCH codes detect up to 4 errors at these
        // lengths), a case change makes the string mixed-case, and a non-
        // alphabet byte is refused. The offset 1 to 255 always changes the
        // byte, so no case is discarded.
        #[test]
        fn one_substitution_is_caught(pos in 0usize..TEST_PK.len(), offset in 1u8..=255) {
            let mut s = TEST_PK.as_bytes().to_vec();
            s[pos] = s[pos].wrapping_add(offset);
            if let Ok(s) = std::str::from_utf8(&s) {
                prop_assert!(decode(s).is_none());
            }
        }

        #[test]
        fn random_strings_never_panic(s in "\\PC{0,100}") {
            let _ = decode(&s);
        }

        #[test]
        fn random_bech32_alphabet(s in "[a-z0-9?-]{1,10}1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{6,60}") {
            if let Some((hrp, data)) = decode(&s) {
                prop_assert_eq!(&*encode(&hrp, &data), &s);
            }
        }
    }
}
