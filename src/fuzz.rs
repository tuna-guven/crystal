//! Entry points for the fuzz targets in fuzz/ (docs/development/testing.md;
//! roadmap step S11b). Compiled only with `cfg(fuzzing)`, which cargo-fuzz
//! sets: Crystal's normal and release builds export nothing from here.
//! Each function takes arbitrary bytes and must never panic, hang or
//! allocate without bound; the round trips also check their results.

use crate::age_v1::{self, Identity, Recipients, x25519};
use crate::md::render::{Kind, render as render_note};
use crate::md::safe::sanitize as sanitize_text;
use crate::vault::{codec, padme};

/// The renderer on hostile note text (A3): the first byte picks the
/// width (0 to 255 columns), the second Markdown or plain text, the rest
/// is the note, as UTF-8 with invalid bytes replaced (notes are UTF-8).
pub fn render(data: &[u8]) {
    let [width, kind, text @ ..] = data else {
        return;
    };
    let kind = if kind & 1 == 0 {
        Kind::Markdown
    } else {
        Kind::Plain
    };
    let text = String::from_utf8_lossy(text);
    let _ = render_note(&text, kind, u16::from(*width));
}

/// The sanitizer on hostile text (A3).
pub fn sanitize(data: &[u8]) {
    let _ = sanitize_text(&String::from_utf8_lossy(data));
}

/// Crystal's age code on hostile files (A1): identity.age and vault.age
/// both pass through `age_v1::decrypt`
/// before anything is authenticated. The first byte picks a mode:
/// 0 decrypts the rest with a fixed X25519 key, 1 with a passphrase whose
/// scrypt work factor is capped low (so a run stays fast); 2 encrypts the
/// rest to the fixed key and must decrypt it back unchanged; 3 encrypts a
/// fixed text, flips the bytes the rest names (position, mask pairs), and
/// decrypts, which reaches the header MAC and the payload stream with
/// otherwise valid files.
pub fn age(data: &[u8]) {
    let [mode, rest @ ..] = data else {
        return;
    };
    let key = x25519::Identity::fixed_for_fuzzing();
    let identity = Identity::X25519(&key);
    match mode % 4 {
        0 => {
            let _ = age_v1::decrypt(&identity, rest);
        }
        1 => {
            let passphrase = Identity::Passphrase {
                passphrase: b"fuzz",
                max_log_n: 4,
            };
            let _ = age_v1::decrypt(&passphrase, rest);
        }
        2 => {
            let recipients = [key.to_public()];
            let file = age_v1::encrypt(&Recipients::X25519(&recipients), rest)
                .expect("encrypting to a valid key cannot fail");
            let back = age_v1::decrypt(&identity, &file).expect("own file must open");
            assert_eq!(&back[..], rest, "round trip changed the plaintext");
        }
        _ => {
            let recipients = [key.to_public()];
            let plain = [0x42; 100];
            let mut file = age_v1::encrypt(&Recipients::X25519(&recipients), &plain)
                .expect("encrypting to a valid key cannot fail");
            let mut changed = false;
            for &[low, high, mask] in rest.as_chunks::<3>().0 {
                let at = usize::from(u16::from_le_bytes([low, high])) % file.len();
                file[at] ^= mask;
                changed |= mask != 0;
            }
            match age_v1::decrypt(&identity, &file) {
                Ok(back) => assert!(!changed || back[..] == plain[..], "forged plaintext"),
                Err(_) => assert!(changed, "an unchanged file did not open"),
            }
        }
    }
}

/// The vault decoder after decryption: Padmé unpadding, then postcard
/// (format 1 or 2) with every limit and name rule (decision 0034).
pub fn vault(data: &[u8]) {
    if let Ok(frame) = padme::unpad(data) {
        let _ = codec::decode(frame);
    }
    let _ = codec::decode(data);
}
