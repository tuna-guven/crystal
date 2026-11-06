//! Padmé padding (DESIGN.md "Vault contents", "Accepted leaks"), for
//! vault.age and config.age. After padding, the ciphertext size reveals
//! only which Padmé bucket the plaintext falls into, not its exact length.
//!
//! Frame, encrypted as a whole by age:
//!
//! ```text
//! u64 little-endian plaintext length || plaintext || zero bytes
//! ```
//!
//! with the zeros filling it up to `padded_len(8 + length)`.
//!
//! Padding is not an integrity check: `unpad` refuses any frame that is not
//! exactly what `pad` makes, but if a plaintext ends in zero bytes, a
//! frame with a smaller length can still look valid. age's authenticated
//! encryption around the frame is what detects tampering.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the vault format and config (#10, #13)")
)]

use std::fmt;

use zeroize::Zeroizing;

/// Size of the length prefix.
const LEN_BYTES: usize = 8;

/// Fixed messages only: never a byte of the frame.
#[derive(Debug, PartialEq, Eq)]
pub enum PadError {
    /// `pad`: the frame would not fit in memory or in u64.
    TooLarge,
    /// `unpad`: shorter than the length prefix.
    TooShort,
    /// `unpad`: the stored length is larger than the bytes after it.
    LengthTooLarge,
    /// `unpad`: the frame is not the Padmé size for the stored length.
    WrongTotal,
    /// `unpad`: a padding byte is not zero.
    NonZeroPadding,
}

impl fmt::Display for PadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "contents are too large to pad",
            Self::TooShort => "padded contents are shorter than their length field",
            Self::LengthTooLarge => "padded contents claim more bytes than they have",
            Self::WrongTotal => "padded contents have the wrong total size",
            Self::NonZeroPadding => "padded contents have non-zero padding",
        })
    }
}

impl std::error::Error for PadError {}

/// Pads `plaintext` into a new frame. The buffer is allocated once with
/// its exact final size (`Vec::with_capacity` guarantees the exact
/// capacity for this), so it never reallocates and leaves no partial
/// copies of the plaintext behind.
pub fn pad(plaintext: &[u8]) -> Result<Zeroizing<Vec<u8>>, PadError> {
    let len = u64::try_from(plaintext.len()).map_err(|_| PadError::TooLarge)?;
    let total = len
        .checked_add(LEN_BYTES as u64)
        .and_then(padded_len)
        .and_then(|t| usize::try_from(t).ok())
        .ok_or(PadError::TooLarge)?;
    let mut frame = Zeroizing::new(Vec::with_capacity(total));
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(plaintext);
    frame.resize(total, 0);
    Ok(frame)
}

/// The plaintext inside a frame, borrowed (no copy). Fails closed: the
/// frame must be exactly what `pad` makes, and nothing is ever repaired.
pub fn unpad(frame: &[u8]) -> Result<&[u8], PadError> {
    let (len_bytes, rest) = frame
        .split_first_chunk::<LEN_BYTES>()
        .ok_or(PadError::TooShort)?;
    let len = u64::from_le_bytes(*len_bytes);
    let len_usize = usize::try_from(len)
        .ok()
        .filter(|&l| l <= rest.len())
        .ok_or(PadError::LengthTooLarge)?;
    // len <= rest.len(), so neither the addition nor padded_len overflows;
    // checked anyway rather than assumed.
    let expected = len
        .checked_add(LEN_BYTES as u64)
        .and_then(padded_len)
        .ok_or(PadError::WrongTotal)?;
    if u64::try_from(frame.len()).ok() != Some(expected) {
        return Err(PadError::WrongTotal);
    }
    let (plaintext, padding) = rest.split_at(len_usize);
    if padding.iter().any(|&b| b != 0) {
        return Err(PadError::NonZeroPadding);
    }
    Ok(plaintext)
}

/// The Padmé length for `n` bytes: `n` rounded up so that only the top
/// `S + 1` bits of the result can be non-zero.
///
/// With `E = ⌊log2 n⌋` and `S = ⌊log2 E⌋ + 1`, the low `E - S` bits are
/// cleared by rounding up:
///
/// ```text
/// mask       = 2^(E - S) - 1
/// padded_len = (n + mask) & !mask
/// ```
///
/// So the result has at most `S + 1 = O(log log n)` significant bits, and
/// the overhead is below `2^-S`: at most 11.7% for any n (the worst case is
/// n = 129, padded to 144) and below 6.25% from n = 256 on (S ≥ 4).
/// Lengths 0 and 1 have no `E`/`S` and stay as they are.
///
/// Nikitin, Barman, Lueks, Underwood, Hubaux, Ford: "Reducing Metadata
/// Leakage from Encrypted Files and Communication with PURBs", PoPETs
/// 2019(4), section 4: <https://doi.org/10.2478/popets-2019-0056>
///
/// `None` only if rounding up overflows u64 (n within 2^57 of u64::MAX),
/// which no real buffer reaches; a length read from a file can, so callers
/// refuse it instead of wrapping.
pub fn padded_len(n: u64) -> Option<u64> {
    if n < 2 {
        return Some(n);
    }
    let e = n.ilog2();
    let s = e.ilog2() + 1;
    let mask = (1u64 << (e - s)) - 1;
    Some(n.checked_add(mask)? & !mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Lengths up to 2^62 never overflow (checked by `no_overflow_below_2_62`).
    const BIG: u64 = 1 << 62;

    fn padded(n: u64) -> u64 {
        padded_len(n).unwrap()
    }

    /// Bits between the highest and the lowest set bit, both included.
    fn significant_bits(x: u64) -> u32 {
        64 - x.leading_zeros() - x.trailing_zeros()
    }

    // Values worked out by hand from the formula.
    #[test]
    fn known_values() {
        let cases = [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (8, 8),
            (9, 10),
            (96, 96),
            (97, 104),
            (129, 144),
            (257, 272),
            (1000, 1024),
            (1_000_000, 1_015_808),
        ];
        for (n, want) in cases {
            assert_eq!(padded(n), want, "n = {n}");
        }
    }

    // Every length up to 2^20: monotonic, never smaller than the input, and
    // at most 11.7% overhead (the doc comment's claim; worst at n = 129).
    #[test]
    fn every_small_length() {
        let mut prev = 0;
        for n in 0..=(1 << 20) {
            let p = padded(n);
            assert!(p >= n && p >= prev, "n = {n}");
            assert!((p - n) * 1000 <= n * 117, "n = {n}, padded = {p}");
            prev = p;
        }
    }

    #[test]
    fn no_overflow_below_2_62_only_near_the_top() {
        assert!(padded_len(BIG).is_some());
        assert!(padded_len(u64::MAX / 2).is_some());
        assert_eq!(padded_len(u64::MAX), None);
    }

    proptest! {
        #[test]
        fn monotonic(a in 0..BIG, b in 0..BIG) {
            let (lo, hi) = (a.min(b), a.max(b));
            prop_assert!(padded(lo) <= padded(hi));
        }

        // Rounding up to a bucket is stable: a bucket size is its own bucket.
        #[test]
        fn idempotent(n in 0..BIG) {
            let p = padded(n);
            prop_assert_eq!(padded(p), p);
        }

        // The issue's bound: at most 12% overhead from 256 bytes up.
        #[test]
        fn overhead_at_most_12_percent(n in 256..BIG) {
            // In u128: n * 12 itself overflows u64 for n above 2^60.
            let (n128, extra) = (u128::from(n), u128::from(padded(n) - n));
            prop_assert!(extra * 100 <= n128 * 12, "n = {}, extra = {}", n, extra);
        }

        // At most S + 1 = ⌊log2 ⌊log2 n⌋⌋ + 2 significant bits.
        #[test]
        fn few_significant_bits(n in 2..BIG) {
            let bound = n.ilog2().ilog2() + 2;
            prop_assert!(significant_bits(padded(n)) <= bound, "n = {}", n);
        }

        // unpad(pad(x)) == x, and the frame is laid out as documented.
        #[test]
        fn round_trip(x in prop::collection::vec(any::<u8>(), 0..4096)) {
            let frame = pad(&x).unwrap();
            check_frame(&frame, &x);
            prop_assert_eq!(unpad(&frame).unwrap(), &x[..]);
        }
    }

    proptest! {
        // Few cases: each builds a buffer of up to 4 MiB. The content is a
        // cheap pattern; padding does not depend on it.
        #![proptest_config(ProptestConfig::with_cases(8))]
        #[test]
        fn round_trip_up_to_4_mib(len in 0usize..(4 << 20), seed in any::<u8>()) {
            let x: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect();
            let frame = pad(&x).unwrap();
            check_frame(&frame, &x);
            prop_assert_eq!(unpad(&frame).unwrap(), &x[..]);
        }
    }

    /// Length prefix, plaintext, zeros, Padmé total, and exactly one
    /// allocation of exactly that size.
    fn check_frame(frame: &[u8], x: &[u8]) {
        let total = padded(LEN_BYTES as u64 + x.len() as u64);
        assert_eq!(frame.len() as u64, total);
        assert_eq!(frame[..LEN_BYTES], (x.len() as u64).to_le_bytes());
        assert_eq!(&frame[LEN_BYTES..LEN_BYTES + x.len()], x);
        assert!(frame[LEN_BYTES + x.len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn pad_allocates_exactly_once() {
        for len in [0, 1, 100, 129, 5000, 100_000] {
            let frame = pad(&vec![7; len]).unwrap();
            assert_eq!(frame.capacity(), frame.len(), "len {len}");
        }
    }

    /// Plaintext length of `sample`: 8 + 100 = 108 pads to 112, so the
    /// frame has 4 padding bytes, and 8 + 99 = 107 pads to 112 too.
    const SAMPLE_LEN: usize = 100;

    /// A frame whose plaintext ends in a non-zero byte and has padding.
    fn sample() -> Zeroizing<Vec<u8>> {
        let x: Vec<u8> = b"secret note "
            .iter()
            .cycle()
            .take(SAMPLE_LEN)
            .copied()
            .collect();
        assert_ne!(x.last(), Some(&0));
        let frame = pad(&x).unwrap();
        assert_eq!(frame.len(), LEN_BYTES + SAMPLE_LEN + 4);
        frame
    }

    #[test]
    fn non_zero_padding_is_refused() {
        let frame = sample();
        for at in LEN_BYTES + SAMPLE_LEN..frame.len() {
            let mut bad = frame.to_vec();
            bad[at] = 1;
            assert_eq!(unpad(&bad), Err(PadError::NonZeroPadding), "byte {at}");
        }
    }

    #[test]
    fn length_larger_than_buffer_is_refused() {
        let frame = sample();
        let room = (frame.len() - LEN_BYTES) as u64;
        for len in [room + 1, u64::from(u32::MAX), u64::MAX] {
            let mut bad = frame.to_vec();
            bad[..LEN_BYTES].copy_from_slice(&len.to_le_bytes());
            assert_eq!(unpad(&bad), Err(PadError::LengthTooLarge), "len {len}");
        }
    }

    // Every cut-off frame is refused: too short for the length prefix, or
    // no longer the Padmé size (or the length no longer fits).
    #[test]
    fn truncated_frame_is_refused() {
        let frame = sample();
        for len in 0..frame.len() {
            let err = unpad(&frame[..len]).unwrap_err();
            let ok = if len < LEN_BYTES {
                err == PadError::TooShort
            } else {
                matches!(err, PadError::WrongTotal | PadError::LengthTooLarge)
            };
            assert!(ok, "length {len}: {err:?}");
        }
    }

    #[test]
    fn extra_bytes_are_refused() {
        let mut bad = sample().to_vec();
        bad.push(0);
        assert_eq!(unpad(&bad), Err(PadError::WrongTotal));
    }

    // Length 99 has the same Padmé total as 100, so only the padding check
    // stands in the way: the last plaintext byte (non-zero) would become
    // padding, and the frame is refused. With a zero last byte it would
    // pass; that is why age's authentication, not padding, guards
    // integrity (module doc).
    #[test]
    fn smaller_length_with_non_zero_tail_is_refused() {
        let mut bad = sample().to_vec();
        let shorter = (SAMPLE_LEN - 1) as u64;
        bad[..LEN_BYTES].copy_from_slice(&shorter.to_le_bytes());
        assert_eq!(unpad(&bad), Err(PadError::NonZeroPadding));
    }

    #[test]
    fn errors_never_show_content() {
        let mut bad = sample().to_vec();
        bad.push(0);
        let err = unpad(&bad).unwrap_err();
        for shown in [err.to_string(), format!("{err:?}")] {
            assert!(!shown.contains("secret"), "{shown}");
        }
    }
}
