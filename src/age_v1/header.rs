//! The age v1 header: version line, recipient stanzas and the `---` MAC
//! line (C2SP age, "Header").
//!
//! Derived from age-core 0.12.0, src/format.rs, and age 0.12.1,
//! src/format.rs and src/keys.rs (https://github.com/str4d/rage), used
//! under the MIT license: Copyright (c) 2019 Jack Grigg
//! (LICENSES/age-MIT.txt).
//!
//! Changes for Crystal: the nom parser and cookie-factory writer are
//! replaced by hand-written code over whole buffers, with the same grammar:
//!
//! ```text
//! header     = "age-encryption.org/v1" LF 1*stanza end-line
//! stanza     = "->" 1*(SP 1*VCHAR) LF *full-line final-line
//! full-line  = 64base64char LF
//! final-line = 0*63base64char LF      ; short, empty if necessary
//! end-line   = "---" SP 43base64char LF
//! ```
//!
//! Bodies are canonical unpadded base64 (`super::base64`). Differences
//! from age 0.12.1 (unchanged since 0.11.5), all toward the spec:
//! - the legacy stanza encodings (a missing body line, or a full last
//!   line with no short line after it) are refused, as the C2SP spec, Go
//!   age and the CCTV vectors `stanza_missing_body` and
//!   `stanza_missing_final_line` require; age 0.12.1 accepts them for
//!   files from its own beta versions, and never writes them;
//! - the header is capped at `MAX_HEADER` bytes and `MAX_STANZAS` stanzas;
//! - no grease stanza is written (optional in the spec; Go age writes
//!   none). Grease and other unknown stanzas are parsed and skipped.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::FILE_KEY_BYTES;
use super::base64;
use super::primitives::hkdf;

const V1_LINE: &[u8] = b"age-encryption.org/v1";
const STANZA_PREFIX: &[u8] = b"-> ";
const MAC_TAG: &[u8] = b"---";
const ENCODED_MAC_LEN: usize = 43;
/// Base64 characters per full body line.
const BODY_COLUMNS: usize = 64;
/// age's HKDF label for the header MAC key.
const HEADER_KEY_LABEL: &[u8] = b"header";

/// The largest header Crystal reads or writes. Crystal's own files have one
/// scrypt stanza or a few X25519 stanzas (well under 1 KiB); this matches
/// the 64 KiB `crypto::MAX_CIPHERTEXT` reserves for the header.
pub(super) const MAX_HEADER: usize = 64 << 10;

/// The most stanzas Crystal reads or writes. Each X25519 stanza costs a
/// Diffie-Hellman operation to try, so a planted header cannot make
/// decryption do thousands.
pub(super) const MAX_STANZAS: usize = 64;

/// A recipient stanza: a type tag, arguments, and a body (a wrapped file
/// key: ciphertext, not secret).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stanza {
    pub(super) tag: String,
    pub(super) args: Vec<String>,
    pub(super) body: Vec<u8>,
}

/// A parsed header, borrowing the bytes its MAC covers.
#[derive(Debug)]
pub(super) struct Header<'a> {
    pub(super) stanzas: Vec<Stanza>,
    mac: [u8; 32],
    /// Everything from the version line up to and including `---`.
    mac_input: &'a [u8],
}

/// age's "arbitrary string": one or more ASCII characters 33 to 126.
fn is_arbitrary_string(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|c| (33..=126).contains(c))
}

/// The next line of `window` from `*pos`, without its LF, and moves past
/// it. None if no LF follows within `window`.
fn next_line<'a>(window: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let rest = window.get(*pos..)?;
    let len = rest.iter().position(|&b| b == b'\n')?;
    *pos += len + 1;
    Some(&rest[..len])
}

/// Parses a stanza whose `-> ` line, without the prefix, is `first`; reads
/// its body lines from `window` at `*pos`.
fn parse_stanza(first: &[u8], window: &[u8], pos: &mut usize) -> Option<Stanza> {
    // "-> " then SP-separated arbitrary strings: an empty part (two
    // spaces, a trailing space, nothing at all) is not one.
    let mut parts = Vec::new();
    for part in first.split(|&b| b == b' ') {
        if !is_arbitrary_string(part) {
            return None;
        }
        // Checked ASCII above, so valid UTF-8.
        parts.push(String::from_utf8(part.to_vec()).ok()?);
    }
    let tag = parts.remove(0);

    // Full 64-column lines, then exactly one shorter line (possibly empty).
    let mut encoded = Vec::new();
    loop {
        let line = next_line(window, pos)?;
        if line.len() > BODY_COLUMNS {
            return None;
        }
        encoded.extend_from_slice(line);
        if line.len() < BODY_COLUMNS {
            break;
        }
    }
    // Rejects non-alphabet bytes, padding and non-canonical endings.
    let body = base64::decode(&encoded)?;
    Some(Stanza {
        tag,
        args: parts,
        body,
    })
}

/// Parses the header at the start of `input`. Returns it and the rest of
/// `input` (the payload). None for anything outside the grammar above,
/// for an unknown version, and past `MAX_HEADER` or `MAX_STANZAS`. The MAC
/// is not checked here: that needs the file key (`Header::verify_mac`).
pub(super) fn parse(input: &[u8]) -> Option<(Header<'_>, &[u8])> {
    let window = &input[..input.len().min(MAX_HEADER)];
    let mut pos = 0;
    if next_line(window, &mut pos)? != V1_LINE {
        return None;
    }
    // Stanzas, until the first line that does not start one.
    let mut stanzas = Vec::new();
    let (end_start, end_line) = loop {
        let line_start = pos;
        let line = next_line(window, &mut pos)?;
        let Some(first) = line.strip_prefix(STANZA_PREFIX) else {
            break (line_start, line);
        };
        if stanzas.len() == MAX_STANZAS {
            return None;
        }
        stanzas.push(parse_stanza(first, window, &mut pos)?);
    };
    // That line must be the end line: "---", SP, the canonical base64 MAC.
    let encoded = end_line.strip_prefix(MAC_TAG)?.strip_prefix(b" ")?;
    if stanzas.is_empty() || encoded.len() != ENCODED_MAC_LEN {
        return None;
    }
    let mac = base64::decode(encoded)?.try_into().ok()?;
    let header = Header {
        stanzas,
        mac,
        mac_input: &input[..end_start + MAC_TAG.len()],
    };
    Some((header, &input[pos..]))
}

/// HMAC-SHA-256 keyed with HKDF(salt empty, "header", file key), as age
/// computes the header MAC. None cannot happen (HMAC takes any key
/// length); it keeps this free of panics.
fn header_mac(file_key: &[u8; FILE_KEY_BYTES]) -> Option<Hmac<Sha256>> {
    let key = hkdf(&[], HEADER_KEY_LABEL, file_key);
    Hmac::<Sha256>::new_from_slice(&*key).ok()
}

impl Header<'_> {
    /// Whether the MAC matches under `file_key`; compared in constant time.
    pub(super) fn verify_mac(&self, file_key: &[u8; FILE_KEY_BYTES]) -> bool {
        let Some(mut mac) = header_mac(file_key) else {
            return false;
        };
        mac.update(self.mac_input);
        mac.verify_slice(&self.mac).is_ok()
    }
}

/// The header up to and including `---`, exactly as age writes it. None
/// for no stanzas, more than `MAX_STANZAS`, or a tag or argument that is
/// not an arbitrary string.
fn write_minus_mac(stanzas: &[Stanza]) -> Option<Vec<u8>> {
    if stanzas.is_empty() || stanzas.len() > MAX_STANZAS {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(V1_LINE);
    out.push(b'\n');
    for stanza in stanzas {
        out.extend_from_slice(STANZA_PREFIX);
        if !is_arbitrary_string(stanza.tag.as_bytes()) {
            return None;
        }
        out.extend_from_slice(stanza.tag.as_bytes());
        for arg in &stanza.args {
            if !is_arbitrary_string(arg.as_bytes()) {
                return None;
            }
            out.push(b' ');
            out.extend_from_slice(arg.as_bytes());
        }
        out.push(b'\n');
        let encoded = base64::encode(&stanza.body);
        for line in encoded.as_bytes().chunks(BODY_COLUMNS) {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
        // The last line must be short: empty if the body fills its lines
        // exactly (including an empty body).
        if encoded.len().is_multiple_of(BODY_COLUMNS) {
            out.push(b'\n');
        }
    }
    out.extend_from_slice(MAC_TAG);
    Some(out)
}

/// The complete header for `stanzas`, with its MAC under `file_key`. None
/// as for `write_minus_mac`, and for a header over `MAX_HEADER`, which
/// Crystal could not read back.
pub(super) fn write(stanzas: &[Stanza], file_key: &[u8; FILE_KEY_BYTES]) -> Option<Vec<u8>> {
    let mut out = write_minus_mac(stanzas)?;
    let mut mac = header_mac(file_key)?;
    mac.update(&out);
    let tag = mac.finalize().into_bytes();
    out.push(b' ');
    out.extend_from_slice(base64::encode(&tag).as_bytes());
    out.push(b'\n');
    (out.len() <= MAX_HEADER).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Any canonical MAC line: parsing does not check the MAC.
    const ANY_MAC: &str = "--- fgMiVLJHMlg9fW7CVG/hPS5EAU4Zeg19LyCP7SoH5nA\n";

    fn header_with(stanzas: &str) -> String {
        format!("age-encryption.org/v1\n{stanzas}{ANY_MAC}")
    }

    /// Parses one stanza as a complete header, and checks it writes back
    /// byte for byte.
    fn one_stanza(text: &str) -> Option<Stanza> {
        let full = header_with(text);
        let (header, rest) = parse(full.as_bytes())?;
        assert!(rest.is_empty());
        assert_eq!(header.stanzas.len(), 1);
        let again = write_minus_mac(&header.stanzas).unwrap();
        assert_eq!(again, header.mac_input);
        header.stanzas.into_iter().next()
    }

    fn stanza(tag: &str, args: &[&str], body: &[u8]) -> Stanza {
        Stanza {
            tag: tag.into(),
            args: args.iter().map(|&a| a.into()).collect(),
            body: body.to_vec(),
        }
    }

    // Ported from age-core 0.12.0, src/format.rs: parse_age_stanza.
    #[test]
    fn parse_age_stanza() {
        // The only body line is short, so no trailing empty line is needed.
        let s = one_stanza(
            "-> X25519 CJM36AHmTbdHSuOQL+NESqyVQE75f2e610iRdLPEN20\n\
             C3ZAeY64NXS4QFrksLm3EGz+uPRyI0eQsWw7LWbbYig\n",
        )
        .unwrap();
        assert_eq!(s.tag, "X25519");
        assert_eq!(s.args, ["CJM36AHmTbdHSuOQL+NESqyVQE75f2e610iRdLPEN20"]);
        assert_eq!(
            s.body,
            base64::decode(b"C3ZAeY64NXS4QFrksLm3EGz+uPRyI0eQsWw7LWbbYig").unwrap()
        );
    }

    // Ported: age_stanza_with_empty_body.
    #[test]
    fn age_stanza_with_empty_body() {
        let s = one_stanza("-> empty-body some arguments\n\n").unwrap();
        assert_eq!(s, stanza("empty-body", &["some", "arguments"], &[]));
    }

    // Ported: age_stanza_with_full_body. A body filling a whole line needs
    // a trailing empty line.
    #[test]
    fn age_stanza_with_full_body() {
        let line = "xD7o4VEOu1t7KZQ1gDgq2FPzBEeSRqbnqvQEXdLRYy143BxR6oFxsUUJCRB0ErXA";
        let s = one_stanza(&format!("-> full-body some arguments\n{line}\n\n")).unwrap();
        assert_eq!(s.body, base64::decode(line.as_bytes()).unwrap());
        assert_eq!(s.body.len(), 48);
    }

    // Ported: age_stanza_with_legacy_full_body. age accepts it through its
    // legacy parser; the spec, Go age and Crystal refuse it.
    #[test]
    fn legacy_full_body_is_refused() {
        let text = "-> full-body some arguments\n\
            xD7o4VEOu1t7KZQ1gDgq2FPzBEeSRqbnqvQEXdLRYy143BxR6oFxsUUJCRB0ErXA\n";
        assert!(parse(header_with(text).as_bytes()).is_none());
        // The legacy missing body, too.
        let text = "-> X25519 CJM36AHmTbdHSuOQL+NESqyVQE75f2e610iRdLPEN20\n";
        assert!(parse(header_with(text).as_bytes()).is_none());
    }

    // Ported: the three cargo-fuzz artifacts (age_stanza_invalid_last_line,
    // _two_trailing_chars, _three_trailing_chars) and base64_padding_rejected.
    #[test]
    fn invalid_body_lines_are_refused() {
        for body in [
            "/\n\n",
            "dy\n\n",
            "ddd\n\n",
            "Tm8gcGFkZGluZyE=\n",
            "SW50ZXJuYWw=UGFk\n",
        ] {
            assert!(one_stanza(&format!("-> H\n{body}")).is_none(), "{body:?}");
        }
        assert!(one_stanza("-> H\nTm8gcGFkZGluZyE\n").is_some());
        assert!(one_stanza("-> H\nSW50ZXJuYWwUGFk\n").is_some());
    }

    // Ported from age 0.12.1, src/format.rs: parse_header. A grease-like
    // mix of stanza kinds, unknown ones included, parses and writes back
    // byte for byte.
    #[test]
    fn parse_header() {
        let text = "age-encryption.org/v1
-> X25519 CJM36AHmTbdHSuOQL+NESqyVQE75f2e610iRdLPEN20
C3ZAeY64NXS4QFrksLm3EGz+uPRyI0eQsWw7LWbbYig
-> X25519 ytazqsbmUnPwVWMVx0c1X9iUtGdY4yAB08UQTY2hNCI
N3pgrXkbIn/RrVt0T0G3sQr1wGWuclqKxTSWHSqGdkc
-> scrypt bBjlhJVYZeE4aqUdmtRHfw 15
ZV/AhotwSGqaPCU43cepl4WYUouAa17a3xpu4G2yi5k
-> ssh-rsa mhir0Q
xD7o4VEOu1t7KZQ1gDgq2FPzBEeSRqbnqvQEXdLRYy143BxR6oFxsUUJCRB0ErXA
mgmZq7tIm5ZyY89OmqZztOgG2tEB1TZvX3Q8oXESBuFjBBQkKaMLkaqh5GjcGRrZ
e5MmTXRdEyNPRl8qpystNZR1q2rEDUHSEJInVLW8OtvQRG8P303VpjnOUU53FSBw
yXxDtzxKxeloceFubn/HWGcR0mHU+1e9l39myQEUZjIoqFIELXvh9o6RUgYzaAI+
m/uPLMQdlIkiOOdbsrE6tFesRLZNHAYspeRKI9MJ++Xg9i7rutU34ZM+1BL6KgZf
J9FSm+GFHiVWpr1MfYCo/w
-> ssh-ed25519 BjH7FA RO+wV4kbbl4NtSmp56lQcfRdRp3dEFpdQmWkaoiw6lY
51eEu5Oo2JYAG7OU4oamH03FDRP18/GnzeCrY7Z+sa8
-> some-empty-body-recipient BjH7FA 37 mhir0Q

-> some-full-body-recipient BjH7FA 37 mhir0Q
xD7o4VEOu1t7KZQ1gDgq2FPzBEeSRqbnqvQEXdLRYy143BxR6oFxsUUJCRB0ErXA

-> some-other-recipient mhir0Q BjH7FA 37
m/uPLMQdlIkiOOdbsrE6tFesRLZNHAYspeRKI9MJ++Xg9i7rutU34ZM+1BL6KgZf
J9FSm+GFHiVWpr1MfYCo/w
--- fgMiVLJHMlg9fW7CVG/hPS5EAU4Zeg19LyCP7SoH5nA
";
        let with_payload = format!("{text}PAYLOAD");
        let (header, rest) = parse(with_payload.as_bytes()).unwrap();
        assert_eq!(rest, b"PAYLOAD");
        assert_eq!(header.stanzas.len(), 8);
        assert_eq!(header.stanzas[2].tag, "scrypt");
        assert_eq!(header.stanzas[2].args, ["bBjlhJVYZeE4aqUdmtRHfw", "15"]);
        let again = write_minus_mac(&header.stanzas).unwrap();
        assert_eq!(again, header.mac_input);
        assert_eq!(
            [
                &again[..],
                b" fgMiVLJHMlg9fW7CVG/hPS5EAU4Zeg19LyCP7SoH5nA\n"
            ]
            .concat(),
            text.as_bytes()
        );
    }

    #[test]
    fn version_and_framing() {
        let ok = header_with("-> a\n\n");
        assert!(parse(ok.as_bytes()).is_some());
        for bad in [
            ok.replace("/v1", "/v2"),
            ok.replace("/v1", "/V1"),
            ok.replace("/v1\n", "/v1 \n"),
            ok.replace('\n', "\r\n"),
            ok.replace("-> a", "->a"),
            ok.replace("-> a", "->  a"),
            ok.replace("-> a", "-> a "),
            ok.replace("-> a", "-> a  b"),
            ok.replace("-> a", "-> a\tb"),
            ok.replace("-> a", "-> a\u{e9}"),
            ok.replace("-> a", "-> "),
            ok.replace("-> a", "->"),
            ok.replace("--- ", "---"),
            ok.replace("--- ", "---  "),
            ok.replace("--- ", "-- "),
            ok.replace("nA\n", "nA"),
            ok.replace("nA\n", "n\n"),
            ok.replace("nA\n", "nAA\n"),
            // Non-canonical MAC: the last character leaves a set bit.
            ok.replace("nA\n", "nB\n"),
            header_with(""),
            String::new(),
            "age-encryption.org/v1".into(),
            "age-encryption.org/v1\n".into(),
            "age-encryption.org/v1\n-> a\n".into(),
        ] {
            assert!(parse(bad.as_bytes()).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn body_lines_must_be_64_then_short() {
        let full = "A".repeat(64);
        assert!(one_stanza(&format!("-> a\n{full}\n{full}\nAA\n")).is_some());
        assert!(one_stanza(&format!("-> a\n{full}\n{full}\n\n")).is_some());
        // A long line, or a short line before the last.
        assert!(one_stanza(&format!("-> a\n{full}A\n\n")).is_none());
        assert!(one_stanza(&format!("-> a\nAA\n{full}\n\n")).is_none());
        // A header whose last body line is never ended.
        assert!(parse(format!("age-encryption.org/v1\n-> a\n{full}\n").as_bytes()).is_none());
    }

    #[test]
    fn stanza_limit() {
        let many = |n: usize| header_with(&"-> a\n\n".repeat(n));
        let at_limit = many(MAX_STANZAS);
        let (header, _) = parse(at_limit.as_bytes()).unwrap();
        assert_eq!(header.stanzas.len(), MAX_STANZAS);
        assert!(parse(many(MAX_STANZAS + 1).as_bytes()).is_none());
        let stanzas = vec![stanza("a", &[], &[]); MAX_STANZAS + 1];
        assert!(write(&stanzas, &[0; 16]).is_none());
        assert!(write(&stanzas[..MAX_STANZAS], &[0; 16]).is_some());
    }

    #[test]
    fn size_limit() {
        // One stanza with a long argument: the header ends at exactly
        // MAX_HEADER bytes, then one more.
        let fixed = header_with("-> a \n\n").len();
        let fits = header_with(&format!("-> a {}\n\n", "x".repeat(MAX_HEADER - fixed)));
        assert_eq!(fits.len(), MAX_HEADER);
        assert!(parse(fits.as_bytes()).is_some());
        let over = header_with(&format!("-> a {}\n\n", "x".repeat(MAX_HEADER - fixed + 1)));
        assert!(parse(over.as_bytes()).is_none());

        let arg = "x".repeat(MAX_HEADER);
        assert!(write(&[stanza("a", &[&arg], &[])], &[0; 16]).is_none());
    }

    #[test]
    fn writer_refuses_invalid_strings() {
        for (tag, args) in [
            ("", &[][..]),
            ("a b", &[]),
            ("a\n", &[]),
            ("\u{e9}", &[]),
            ("a", &[""][..]),
            ("a", &["b c"]),
        ] {
            assert!(
                write(&[stanza(tag, args, &[])], &[0; 16]).is_none(),
                "{tag:?}"
            );
        }
        assert!(write(&[], &[0; 16]).is_none());
    }

    #[test]
    fn mac_round_trip() {
        let stanzas = [
            stanza(
                "X25519",
                &["CJM36AHmTbdHSuOQL+NESqyVQE75f2e610iRdLPEN20"],
                &[7; 32],
            ),
            stanza("b-grease", &["!", "~"], &[1; 64]),
        ];
        let key = [42; 16];
        let bytes = write(&stanzas, &key).unwrap();
        let (header, rest) = parse(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(header.stanzas, stanzas);
        assert!(header.verify_mac(&key));
        assert!(!header.verify_mac(&[43; 16]));
    }

    // The MAC is HMAC-SHA-256 under HKDF-SHA-256(salt "", info "header",
    // file key), over the header through "---" (C2SP age). Recomputed here
    // with the RustCrypto crates directly, independent of `header_mac`.
    #[test]
    fn mac_construction_matches_spec() {
        let key = [9; 16];
        let bytes = write(&[stanza("a", &[], &[1, 2, 3])], &key).unwrap();
        let mut okm = [0; 32];
        ::hkdf::Hkdf::<Sha256>::new(Some(&[]), &key)
            .expand(b"header", &mut okm)
            .unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&okm).unwrap();
        let cut = bytes.len() - ENCODED_MAC_LEN - 2;
        mac.update(&bytes[..cut]);
        let expected = base64::encode(&mac.finalize().into_bytes());
        assert_eq!(&bytes[cut..cut + 1], b" ");
        assert_eq!(&bytes[cut + 1..bytes.len() - 1], expected.as_bytes());
    }

    // Headers written by the age crate (0.11.5, what existing vaults were
    // written with; tests/corpus/age-rage/) parse, and write back byte for
    // byte: X25519 stanzas, then age's random grease stanza, which is
    // skipped as unknown.
    #[test]
    fn parses_age_crate_headers() {
        let files = crate::age_v1::rage::x25519_files();
        assert_eq!(files.len(), 12);
        for (name, file) in files {
            let recipients = if name.starts_with("x25519-3-") { 3 } else { 1 };
            let (header, payload) = parse(&file).unwrap();
            assert_eq!(header.stanzas.len(), recipients + 1, "{name}");
            for stanza in &header.stanzas[..recipients] {
                assert_eq!(stanza.tag, "X25519", "{name}");
            }
            assert!(
                header.stanzas[recipients].tag.ends_with("-grease"),
                "{name}"
            );
            // The 16-byte nonce and at least one chunk's 16-byte tag.
            assert!(payload.len() >= 16 + 16, "{name}");
            let again = write_minus_mac(&header.stanzas).unwrap();
            assert_eq!(again, header.mac_input, "{name}");
        }
    }

    fn arbitrary_string() -> impl Strategy<Value = String> {
        "[!-~]{1,12}"
    }

    fn any_stanza() -> impl Strategy<Value = Stanza> {
        (
            arbitrary_string(),
            proptest::collection::vec(arbitrary_string(), 0..4),
            proptest::collection::vec(any::<u8>(), 0..200),
        )
            .prop_map(|(tag, args, body)| Stanza { tag, args, body })
    }

    proptest! {
        #[test]
        fn round_trip(
            stanzas in proptest::collection::vec(any_stanza(), 1..6),
            key in any::<[u8; 16]>(),
            payload in proptest::collection::vec(any::<u8>(), 0..40),
        ) {
            let bytes = write(&stanzas, &key).unwrap();
            let file = [&bytes[..], &payload[..]].concat();
            let (header, rest) = parse(&file).unwrap();
            prop_assert_eq!(rest, &payload[..]);
            prop_assert_eq!(&header.stanzas, &stanzas);
            prop_assert!(header.verify_mac(&key));
        }

        // Any one flipped bit in the header: it no longer parses, or its
        // MAC fails.
        #[test]
        fn flipped_bit_is_caught(
            stanzas in proptest::collection::vec(any_stanza(), 1..4),
            key in any::<[u8; 16]>(),
            bit in any::<prop::sample::Index>(),
        ) {
            let mut bytes = write(&stanzas, &key).unwrap();
            let bit = bit.index(bytes.len() * 8);
            bytes[bit / 8] ^= 1 << (bit % 8);
            if let Some((header, _)) = parse(&bytes) {
                prop_assert!(!header.verify_mac(&key));
            }
        }

        #[test]
        fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = parse(&bytes);
            let prefixed = [&b"age-encryption.org/v1\n-> "[..], &bytes[..]].concat();
            let _ = parse(&prefixed);
        }
    }
}
