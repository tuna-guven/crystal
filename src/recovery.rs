//! The vault's recovery code (docs/decisions/0029-recovery-code.md).
//!
//! `crystal init` generates it, shows it once to be written on paper, and
//! seals `identity.backup.age` with it; `crystal recover` takes it back to
//! open the vault without the passphrase. Crystal makes it itself, so no
//! other program is needed.
//!
//! 27 random symbols (135 bits from the kernel's generator) and one check
//! symbol, in Crockford's base32 alphabet: digits and capitals without I,
//! L, O and U. Shown as 7 groups of 4 (`K7QM-3XRD-…`). Read back leniently:
//! any case, dashes and spaces anywhere, O as 0, I and L as 1.
//!
//! The canonical 28 symbols are age's scrypt passphrase for
//! `identity.backup.age`. 135 random bits cannot be guessed, whatever the
//! scrypt cost.

use std::fmt;

use rustix::io::Errno;
use zeroize::Zeroizing;

use crate::crypto::{Passphrase, fill_random};
use crate::model::assert_not_impl;

/// Random symbols in a code.
const RANDOM_SYMBOLS: usize = 27;
/// Symbols in a code: the random ones and the check symbol.
pub const SYMBOLS: usize = RANDOM_SYMBOLS + 1;
/// Symbols per group when shown.
const GROUP: usize = 4;
/// Crockford's base32: value `i` is `ALPHABET[i]`.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A recovery code: the 28 canonical symbols, wiped on drop. No Debug,
/// Display, Clone or Serialize; it leaves only through `grouped` (to be
/// shown once) and `passphrase` (for age).
pub struct RecoveryCode(Zeroizing<[u8; SYMBOLS]>);

assert_not_impl!(RecoveryCode: fmt::Debug, fmt::Display, Clone, serde::Serialize);

/// Why a typed code was refused. Fixed text; never what was typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeError {
    /// Not 28 symbols once dashes and spaces are dropped.
    WrongLength,
    /// A character outside the alphabet (and not a dash, space, O, I or L).
    BadCharacter,
    /// The check symbol does not match: a symbol was mistyped.
    Mistyped,
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength => write!(
                f,
                "a recovery code has {SYMBOLS} characters (7 groups of 4)"
            ),
            Self::BadCharacter => write!(
                f,
                "a recovery code has only digits and the letters A to Z without U"
            ),
            Self::Mistyped => write!(
                f,
                "that recovery code has a typo (its check character does not match)"
            ),
        }
    }
}

impl std::error::Error for CodeError {}

impl RecoveryCode {
    /// A new code from the kernel's generator.
    pub fn generate() -> Result<Self, Errno> {
        // 136 bits; the first 135 become the 27 random symbols.
        let mut random = Zeroizing::new([0u8; 17]);
        fill_random(random.as_mut())?;
        let mut values = Zeroizing::new([0u8; SYMBOLS]);
        for (i, value) in values[..RANDOM_SYMBOLS].iter_mut().enumerate() {
            *value = bits5(random.as_ref(), i * 5);
        }
        values[RANDOM_SYMBOLS] = check_value(&values[..RANDOM_SYMBOLS]);
        Ok(Self::from_values(&values))
    }

    /// Reads a typed code: any case, dashes and spaces anywhere, O read as
    /// 0 and I or L as 1. The check symbol must match.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let mut values = Zeroizing::new([0u8; SYMBOLS]);
        let mut count = 0;
        for byte in text.bytes() {
            if byte == b'-' || byte == b' ' {
                continue;
            }
            let value = symbol_value(byte).ok_or(CodeError::BadCharacter)?;
            let slot = values.get_mut(count).ok_or(CodeError::WrongLength)?;
            *slot = value;
            count += 1;
        }
        if count != SYMBOLS {
            return Err(CodeError::WrongLength);
        }
        if check_value(&values[..RANDOM_SYMBOLS]) != values[RANDOM_SYMBOLS] {
            return Err(CodeError::Mistyped);
        }
        Ok(Self::from_values(&values))
    }

    fn from_values(values: &[u8; SYMBOLS]) -> Self {
        let mut symbols = Zeroizing::new([0u8; SYMBOLS]);
        for (symbol, &value) in symbols.iter_mut().zip(values) {
            *symbol = ALPHABET[usize::from(value)];
        }
        Self(symbols)
    }

    /// The code as shown: 7 groups of 4 joined by dashes. Built with its
    /// final capacity, so it never reallocates.
    pub fn grouped(&self) -> Zeroizing<String> {
        let groups = SYMBOLS / GROUP;
        let mut text = Zeroizing::new(String::with_capacity(SYMBOLS + groups - 1));
        for (i, &symbol) in self.0.iter().enumerate() {
            if i > 0 && i % GROUP == 0 {
                text.push('-');
            }
            text.push(char::from(symbol));
        }
        text
    }

    /// The passphrase age's scrypt uses for `identity.backup.age`: the 28
    /// canonical symbols, so every spelling of the code opens the file.
    pub fn passphrase(&self) -> Passphrase {
        // 28 ASCII symbols: never empty, always UTF-8.
        Passphrase::new(Zeroizing::new(self.0.to_vec()))
            .unwrap_or_else(|| unreachable!("a code is 28 ASCII symbols"))
    }
}

/// The 5 bits of `bytes` starting at bit `at` (most significant first).
fn bits5(bytes: &[u8], at: usize) -> u8 {
    let word = u16::from_be_bytes([bytes[at / 8], bytes.get(at / 8 + 1).copied().unwrap_or(0)]);
    let shift = 16 - 5 - (at % 8);
    // Masked to 5 bits: always fits.
    u8::try_from((word >> shift) & 0x1f).unwrap_or(0)
}

/// The check symbol's value: the sum of each value times an odd weight
/// (1, 3, 5, ...), modulo 32. Odd weights are invertible modulo 32, so any
/// single wrong symbol changes the sum; two neighbouring symbols swapped
/// change it unless their values differ by exactly 16.
fn check_value(values: &[u8]) -> u8 {
    let sum = values
        .iter()
        .enumerate()
        .fold(0usize, |sum, (i, &v)| sum + (2 * i + 1) * usize::from(v));
    // Modulo 32: always fits.
    u8::try_from(sum % 32).unwrap_or(0)
}

/// The value of a typed symbol, with Crockford's readings of O, I and L.
fn symbol_value(byte: u8) -> Option<u8> {
    let upper = match byte.to_ascii_uppercase() {
        b'O' => b'0',
        b'I' | b'L' => b'1',
        other => other,
    };
    ALPHABET
        .iter()
        .position(|&s| s == upper)
        .and_then(|i| u8::try_from(i).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(code: &RecoveryCode) -> String {
        String::from_utf8(code.0.to_vec()).unwrap()
    }

    #[test]
    fn generated_codes_are_well_formed_and_differ() {
        let a = RecoveryCode::generate().unwrap();
        let b = RecoveryCode::generate().unwrap();
        assert_ne!(symbols(&a), symbols(&b));
        let grouped = a.grouped();
        assert_eq!(grouped.len(), 34);
        let groups: Vec<&str> = grouped.split('-').collect();
        assert_eq!(groups.len(), 7);
        assert!(groups.iter().all(|g| g.len() == 4));
        assert!(grouped.bytes().all(|c| c == b'-' || ALPHABET.contains(&c)));
        assert_eq!(
            symbols(&RecoveryCode::parse(&grouped).unwrap()),
            symbols(&a)
        );
    }

    // Every symbol value turns up: the random bits reach all 5 bits of
    // every position (no position stuck, no value impossible).
    #[test]
    fn random_symbols_use_the_whole_alphabet() {
        let mut seen = [[false; 32]; RANDOM_SYMBOLS];
        for _ in 0..2000 {
            let code = RecoveryCode::generate().unwrap();
            for (i, &symbol) in code.0[..RANDOM_SYMBOLS].iter().enumerate() {
                seen[i][usize::from(symbol_value(symbol).unwrap())] = true;
            }
        }
        assert!(seen.iter().all(|row| row.iter().all(|&s| s)));
    }

    #[test]
    fn bits_are_read_in_order() {
        let bytes = [0b1010_1100, 0b0111_0001, 0b1000_0000];
        assert_eq!(bits5(&bytes, 0), 0b10101);
        assert_eq!(bits5(&bytes, 5), 0b10001);
        assert_eq!(bits5(&bytes, 10), 0b11000);
        assert_eq!(bits5(&bytes, 15), 0b11000);
    }

    #[test]
    fn typing_is_lenient() {
        let code = RecoveryCode::generate().unwrap();
        let grouped = code.grouped().to_string();
        let canonical = symbols(&code);
        let spellings = [
            grouped.to_lowercase(),
            canonical.clone(),
            grouped.replace('-', " "),
            format!(" {} ", grouped.replace('-', " - ")),
            canonical.replace('0', "O").replace('1', "l"),
            canonical.replace('0', "o").replace('1', "I"),
        ];
        for spelling in spellings {
            let parsed = RecoveryCode::parse(&spelling).unwrap();
            assert_eq!(symbols(&parsed), canonical, "{spelling}");
        }
    }

    // Exhaustive: every single-symbol mistake is caught.
    #[test]
    fn every_single_typo_is_caught() {
        let code = RecoveryCode::generate().unwrap();
        let canonical = symbols(&code);
        for i in 0..SYMBOLS {
            for &wrong in ALPHABET.iter().filter(|&&s| s != code.0[i]) {
                let mut typed = canonical.clone().into_bytes();
                typed[i] = wrong;
                let typed = String::from_utf8(typed).unwrap();
                assert_eq!(
                    RecoveryCode::parse(&typed).err(),
                    Some(CodeError::Mistyped),
                    "{i}"
                );
            }
        }
    }

    #[test]
    fn neighbours_swapped_are_caught_unless_16_apart() {
        let code = RecoveryCode::generate().unwrap();
        let canonical = symbols(&code);
        for i in 0..SYMBOLS - 1 {
            let (a, b) = (code.0[i], code.0[i + 1]);
            let mut typed = canonical.clone().into_bytes();
            typed.swap(i, i + 1);
            let typed = String::from_utf8(typed).unwrap();
            let apart = symbol_value(a).unwrap().abs_diff(symbol_value(b).unwrap());
            let result = RecoveryCode::parse(&typed);
            // The documented blind spot, also for the last random symbol
            // swapped with the check symbol.
            if a == b || apart == 16 {
                continue;
            }
            assert_eq!(result.err(), Some(CodeError::Mistyped), "{i}");
        }
    }

    #[test]
    fn bad_lengths_and_characters_are_refused() {
        let code = RecoveryCode::generate().unwrap();
        let canonical = symbols(&code);
        assert_eq!(
            RecoveryCode::parse(&canonical[1..]).err(),
            Some(CodeError::WrongLength)
        );
        assert_eq!(
            RecoveryCode::parse(&format!("{canonical}0")).err(),
            Some(CodeError::WrongLength)
        );
        assert_eq!(RecoveryCode::parse("").err(), Some(CodeError::WrongLength));
        for bad in ["U", "u", "_", "é", "*"] {
            let typed = format!("{bad}{}", &canonical[1..]);
            assert_eq!(
                RecoveryCode::parse(&typed).err(),
                Some(CodeError::BadCharacter),
                "{bad}"
            );
        }
    }

    #[test]
    fn errors_never_show_the_code() {
        for e in [
            CodeError::WrongLength,
            CodeError::BadCharacter,
            CodeError::Mistyped,
        ] {
            let text = e.to_string();
            assert!(!text.contains('-'), "{text}");
        }
    }

    // Every spelling gives the same age passphrase, so it opens the same
    // identity.backup.age.
    #[test]
    fn passphrase_is_the_canonical_symbols() {
        let code = RecoveryCode::generate().unwrap();
        let lower = RecoveryCode::parse(&code.grouped().to_lowercase()).unwrap();
        assert!(code.passphrase().matches(&lower.passphrase()));
        assert_eq!(code.passphrase().len_for_test(), SYMBOLS);
    }

    // Decision 0029: the passphrase is the code as shown, without its
    // dashes, so any age tool opens identity.backup.age with it.
    #[test]
    fn passphrase_is_the_shown_code_without_dashes() {
        for _ in 0..50 {
            let code = RecoveryCode::generate().unwrap();
            let typed = Zeroizing::new(code.grouped().replace('-', "").into_bytes());
            let typed = Passphrase::new(typed).unwrap();
            assert!(code.passphrase().matches(&typed));
        }
    }
}
