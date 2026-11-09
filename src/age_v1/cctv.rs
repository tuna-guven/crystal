//! The C2SP/CCTV age test vectors (tests/corpus/age-cctv/, imported by
//! scripts/import-cctv.sh; README: https://github.com/C2SP/CCTV/tree/main/age).
//!
//! Every vector for what Crystal implements (X25519, scrypt, the header
//! and its MAC, STREAM) must give its expected result. Armored and hybrid
//! (post-quantum) vectors are skipped, as the README allows for
//! unimplemented features. Errors map to the README's outcomes as age's
//! own testkit maps them (age 0.12.1, tests/testkit.rs):
//!
//! | expect          | `decrypt` result                    |
//! |-----------------|-------------------------------------|
//! | success         | Ok, and the payload's SHA-256       |
//! | no match        | `NoMatch` or `DecryptionFailed`     |
//! | HMAC failure    | `InvalidMac`                        |
//! | header failure  | `InvalidHeader` or `ExcessiveWork`  |
//! | payload failure | `InvalidPayload`                    |
//!
//! For a payload failure the README also checks the plaintext a streaming
//! API released before the error. `decrypt` works on whole buffers and
//! releases nothing unless the whole payload authenticates, so only the
//! failure is checked.
//!
//! As the README suggests, the vectors' file keys also test the encoding
//! direction: every vector whose header parses must re-encode to the same
//! bytes, and every success vector's plaintext must re-encrypt to the same
//! payload.

use sha2::{Digest, Sha256};

use super::{Error, FILE_KEY_BYTES, Identity, decrypt, header, stream, x25519};

/// Crystal's largest accepted scrypt work factor (crypto.rs, decision
/// 0027): log N 18 + 4. CCTV's `scrypt_work_factor_23` must be refused.
const MAX_LOG_N: u8 = 22;

/// One vector: its textual header, then the age file.
struct Vector {
    name: String,
    expect: String,
    payload_sha256: Option<Vec<u8>>,
    /// Hex as given; the `*_long_file_key` vectors use 25 bytes on purpose.
    file_key: Option<Vec<u8>>,
    identities: Vec<String>,
    passphrases: Vec<String>,
    armored: bool,
    file: Vec<u8>,
}

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn read_vector(path: &std::path::Path) -> Vector {
    let data = std::fs::read(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let split = data
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or_else(|| panic!("{name}: no empty line"));
    let mut vector = Vector {
        name: name.clone(),
        expect: String::new(),
        payload_sha256: None,
        file_key: None,
        identities: Vec::new(),
        passphrases: Vec::new(),
        armored: false,
        file: data[split + 2..].to_vec(),
    };
    for line in String::from_utf8_lossy(&data[..split]).lines() {
        let (key, value) = line
            .split_once(": ")
            .unwrap_or_else(|| panic!("{name}: bad line {line:?}"));
        match key {
            "expect" => vector.expect = value.to_owned(),
            "payload" => vector.payload_sha256 = Some(hex(value)),
            "file key" => vector.file_key = Some(hex(value)),
            "identity" => vector.identities.push(value.to_owned()),
            "passphrase" => vector.passphrases.push(value.to_owned()),
            "armored" => vector.armored = value == "yes",
            "compressed" => {
                assert_eq!(value, "zlib", "{name}: compression");
                // CCTV compresses some large files; the largest is about
                // 17 MB inflated, so 64 MiB is a generous cap.
                vector.file =
                    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&vector.file, 64 << 20)
                        .unwrap_or_else(|e| panic!("{name}: zlib: {e:?}"));
            }
            "comment" => {}
            // Any other key is new to this runner.
            _ => panic!("{name}: unknown key {key:?}; update cctv.rs"),
        }
    }
    vector
}

/// Armor and the hybrid (ML-KEM) recipient are not implemented.
fn skipped(v: &Vector) -> bool {
    v.armored
        || v.expect == "armor failure"
        || v.name.starts_with("hybrid")
        || v.identities
            .iter()
            .any(|i| i.starts_with("AGE-SECRET-KEY-PQ-"))
}

/// Decrypts as age does with several identities: each in turn until one
/// answers (anything but "no stanza for me" ends the search).
fn decrypt_vector(v: &Vector) -> Result<zeroize::Zeroizing<Vec<u8>>, Error> {
    let keys: Vec<x25519::Identity> = v
        .identities
        .iter()
        .map(|s| x25519::Identity::parse(s).unwrap_or_else(|| panic!("{}: identity", v.name)))
        .collect();
    let mut identities: Vec<Identity<'_>> = keys.iter().map(Identity::X25519).collect();
    identities.extend(v.passphrases.iter().map(|p| Identity::Passphrase {
        passphrase: p.as_bytes(),
        max_log_n: MAX_LOG_N,
    }));
    // A vector without identities still has its header parsed.
    let stranger = x25519::Identity::generate().unwrap();
    if identities.is_empty() {
        identities.push(Identity::X25519(&stranger));
    }
    let mut result = Err(Error::NoMatch);
    for identity in &identities {
        result = decrypt(identity, &v.file);
        if result.as_ref().err() != Some(&Error::NoMatch) {
            break;
        }
    }
    result
}

/// The vector's header re-encodes to the same bytes under its file key,
/// and a success vector's plaintext re-encrypts to the same payload.
fn check_encoding(v: &Vector, plaintext: Option<&[u8]>) {
    let Some((parsed, rest)) = header::parse(&v.file) else {
        return;
    };
    // Only a real 16-byte file key can re-encode.
    let Some(file_key) = v
        .file_key
        .as_deref()
        .and_then(|k| <[u8; FILE_KEY_BYTES]>::try_from(k).ok())
    else {
        return;
    };
    let header_len = v.file.len() - rest.len();
    if v.expect != "HMAC failure" {
        let again = header::write(&parsed.stanzas, &file_key).expect("re-encode");
        assert_eq!(again, v.file[..header_len], "{}: header round trip", v.name);
    }
    if let (Some(plaintext), Some((nonce, payload))) = (plaintext, rest.split_first_chunk()) {
        let key = stream::payload_key(&file_key, nonce);
        let again = stream::encrypt(&key, plaintext).unwrap();
        assert!(again == payload, "{}: STREAM round trip", v.name);
    }
}

#[test]
fn cctv_age_vectors() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/age-cctv");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap() != "SOURCE")
        .collect();
    paths.sort();

    let (mut ran, mut skip) = (0, 0);
    for path in &paths {
        let v = read_vector(path);
        if skipped(&v) {
            skip += 1;
            continue;
        }
        ran += 1;
        let result = decrypt_vector(&v);
        let ok = match (v.expect.as_str(), &result) {
            ("success", Ok(plaintext)) => {
                let hash = Sha256::digest(&plaintext[..]);
                assert_eq!(
                    Some(hash.to_vec()),
                    v.payload_sha256,
                    "{}: payload hash",
                    v.name
                );
                check_encoding(&v, Some(plaintext));
                true
            }
            ("no match", Err(Error::NoMatch | Error::DecryptionFailed))
            | ("HMAC failure", Err(Error::InvalidMac))
            | ("header failure", Err(Error::InvalidHeader | Error::ExcessiveWork))
            | ("payload failure", Err(Error::InvalidPayload)) => {
                check_encoding(&v, None);
                true
            }
            _ => false,
        };
        assert!(
            ok,
            "{}: expected {}, got {:?}",
            v.name,
            v.expect,
            result.as_ref().map(|p| p.len())
        );
    }
    // The corpus at the pinned commit: 147 vectors, 37 armored and 19
    // hybrid (armor_hybrid is both), so 92 for what Crystal implements.
    assert_eq!(ran + skip, 147, "vector count changed: re-check skips");
    assert_eq!(ran, 92, "vectors run");
}
