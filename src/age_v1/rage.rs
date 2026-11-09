//! Files written by the age crate 0.11.5 (rage's library, what Crystal
//! linked before step C2), stored in tests/corpus/age-rage/ before step C3
//! removed the crate (see its SOURCE note; decisions 0042 and 0043).
//!
//! They replace the cross-checks that called the age crate:
//! - reading: every file opens with each of its keys or its passphrase and
//!   gives the plaintext in MANIFEST; a stranger's key or a wrong
//!   passphrase does not open it;
//! - writing: under the file key Crystal unwraps, each file's stanzas
//!   (age's, re-serialized, grease included), header MAC and STREAM
//!   payload re-encode to age's bytes exactly. The stanzas themselves are
//!   age's: a new stanza's share or salt is random, so `wrap` is checked
//!   another way. It shares `wrapping_key` with `unwrap`, which these
//!   files and the CCTV vectors check, and the round trips open every
//!   stanza `wrap` makes with that `unwrap`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{Error, FileKey, Identity, decrypt, header, scrypt, stream, x25519};

/// Crystal's test work-factor cap (crypto.rs: `TEST_SCRYPT_LOG_N` 10, + 4).
const MAX_LOG_N: u8 = 14;

/// One line of MANIFEST.
struct Fixture {
    name: String,
    /// X25519 key names (key0..key2), empty for a scrypt file.
    keys: Vec<String>,
    /// The passphrase and its log N, for a scrypt file.
    scrypt: Option<(String, u8)>,
    len: usize,
    sha256: String,
    file: Vec<u8>,
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/age-rage")
}

/// keys.txt, the one reader of it: per line the key's name, its secret
/// key string and its recipient, as age printed them (tabs between).
fn key_lines() -> Vec<(String, Zeroizing<String>, String)> {
    let text = Zeroizing::new(std::fs::read_to_string(dir().join("keys.txt")).unwrap());
    text.lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, secret, recipient] = fields[..] else {
                panic!("keys.txt: bad line");
            };
            (
                name.to_owned(),
                Zeroizing::new(secret.to_owned()),
                recipient.to_owned(),
            )
        })
        .collect()
}

/// Each key's name, the key as Crystal parses it, and its secret string.
fn keys() -> Vec<(String, x25519::Identity, Zeroizing<String>)> {
    key_lines()
        .into_iter()
        .map(|(name, secret, _recipient)| {
            let key = x25519::Identity::parse(&secret).expect("keys.txt: secret key");
            (name, key, secret)
        })
        .collect()
}

fn fixtures() -> Vec<Fixture> {
    let text = std::fs::read_to_string(dir().join("MANIFEST")).unwrap();
    text.lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, kind, who, log_n, len, sha256] = fields[..] else {
                panic!("MANIFEST: bad line {line:?}");
            };
            let (keys, scrypt) = match kind {
                "x25519" => (who.split(',').map(str::to_owned).collect(), None),
                "scrypt" => (Vec::new(), Some((who.to_owned(), log_n.parse().unwrap()))),
                _ => panic!("{name}: unknown kind {kind:?}"),
            };
            Fixture {
                name: name.to_owned(),
                keys,
                scrypt,
                len: len.parse().unwrap(),
                sha256: sha256.to_owned(),
                file: std::fs::read(dir().join(name)).unwrap(),
            }
        })
        .collect()
}

/// keys.txt as age printed it: each secret key string and its recipient.
pub(crate) fn key_strings() -> Vec<(Zeroizing<String>, String)> {
    key_lines()
        .into_iter()
        .map(|(_name, secret, recipient)| (secret, recipient))
        .collect()
}

/// One fixture file's bytes.
pub(crate) fn file(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap()
}

/// The X25519 files (name and bytes): age 0.11.5 wrote each with one
/// random grease stanza after the recipients' stanzas.
pub(crate) fn x25519_files() -> Vec<(String, Vec<u8>)> {
    fixtures()
        .into_iter()
        .filter(|f| f.scrypt.is_none())
        .map(|f| (f.name, f.file))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The X25519 files' plaintext, as the generator made it.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 253) as u8).collect()
}

fn key<'k>(
    keys: &'k [(String, x25519::Identity, Zeroizing<String>)],
    name: &str,
) -> &'k x25519::Identity {
    &keys.iter().find(|(n, _, _)| n == name).unwrap().1
}

fn check_plaintext(f: &Fixture, plaintext: &[u8]) {
    assert_eq!(plaintext.len(), f.len, "{}: length", f.name);
    assert_eq!(
        hex(&Sha256::digest(plaintext)),
        f.sha256,
        "{}: SHA-256",
        f.name
    );
}

#[test]
fn age_rage_files_open() {
    let keys = keys();
    let all = fixtures();
    // 5 scrypt and 12 X25519 files (6 sizes, to 1 and to 3 keys).
    assert_eq!(all.len(), 17, "fixture count changed");
    let stranger = x25519::Identity::generate().unwrap();
    for f in &all {
        if let Some((pass, log_n)) = &f.scrypt {
            let by_pass = Identity::Passphrase {
                passphrase: pass.as_bytes(),
                max_log_n: MAX_LOG_N,
            };
            let result = decrypt(&by_pass, &f.file);
            if *log_n > MAX_LOG_N {
                assert_eq!(result.err(), Some(Error::ExcessiveWork), "{}", f.name);
                continue;
            }
            check_plaintext(f, &result.unwrap());
            let wrong = Identity::Passphrase {
                passphrase: b"wrong",
                max_log_n: MAX_LOG_N,
            };
            let err = decrypt(&wrong, &f.file).err();
            assert_eq!(err, Some(Error::DecryptionFailed), "{}", f.name);
        } else {
            // The manifest's hash is of the generator's pattern.
            check_plaintext(f, &pattern(f.len));
            for name in &f.keys {
                let plaintext = decrypt(&Identity::X25519(key(&keys, name)), &f.file).unwrap();
                check_plaintext(f, &plaintext);
            }
            let err = decrypt(&Identity::X25519(&stranger), &f.file).err();
            assert_eq!(err, Some(Error::NoMatch), "{}", f.name);
        }
    }
    // The identity files hold key0's secret key string, as age printed it.
    let identity = all
        .iter()
        .find(|f| f.name == "scrypt-identity.age")
        .unwrap();
    let plain = decrypt(
        &Identity::Passphrase {
            passphrase: b"correct horse",
            max_log_n: MAX_LOG_N,
        },
        &identity.file,
    )
    .unwrap();
    assert_eq!(plain[..], *keys[0].2.as_bytes());
}

/// The file key of `f`, unwrapped by Crystal from the file's first stanza
/// its key or passphrase answers.
fn file_key(
    f: &Fixture,
    keys: &[(String, x25519::Identity, Zeroizing<String>)],
    stanzas: &[header::Stanza],
) -> FileKey {
    stanzas
        .iter()
        .find_map(|s| match &f.scrypt {
            // The file's own log N: re-encoding costs at most log N 15.
            Some((pass, log_n)) => scrypt::unwrap(s, pass.as_bytes(), *log_n),
            None => key(keys, &f.keys[0]).unwrap(s),
        })
        .unwrap()
        .unwrap()
}

#[test]
fn age_rage_files_re_encode_byte_for_byte() {
    let keys = keys();
    let all = fixtures();
    assert_eq!(all.len(), 17, "fixture count changed");
    for f in all {
        let (parsed, rest) = header::parse(&f.file).unwrap();
        let key = file_key(&f, &keys, &parsed.stanzas);
        let header_len = f.file.len() - rest.len();
        // Same stanzas (age's grease included) and MAC, same bytes.
        let again = header::write(&parsed.stanzas, key.expose()).unwrap();
        assert_eq!(again, f.file[..header_len], "{}: header", f.name);
        // Same plaintext under the file's nonce: the same STREAM payload.
        let (nonce, payload) = rest.split_first_chunk().unwrap();
        let payload_key = stream::payload_key(key.expose(), nonce);
        let plaintext = stream::decrypt(&payload_key, payload).unwrap();
        let again = stream::encrypt(&payload_key, &plaintext).unwrap();
        assert!(again == payload, "{}: payload", f.name);
    }
}
