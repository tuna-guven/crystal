# 0042 Crystal's own implementation of the age v1 format

- **Date:** 2026-11-09 (step C1, before Phase 4)
- **Status:** Accepted. Replaces 0002 (age only, linked as a library) and
  0015 (age's random source must be allowed by seccomp), now removed; what
  still holds of both is restated below. Changes the upgrade path in
  [0003](0003-no-post-quantum-yet.md).

## Context

Every file Crystal writes is an age file, and age is the right format: a
small, published specification ([C2SP age](https://c2sp.org/age)) with
several independent implementations and a shared test suite (C2SP/CCTV).
Crystal used it through the `age` crate, rage's library.

That crate is much larger than what Crystal uses. Crystal needs two
recipient types (X25519 and scrypt) over whole buffers of at most 32 MiB;
the crate also brings armor, plugins, SSH keys, async and streaming I/O,
and translations. It accounted for 102 of Crystal's 133 runtime crates,
including build-time proc macros with advisories (RUSTSEC-2026-0374 in
`basic-toml`, RUSTSEC-2026-0173 in `proc-macro-error2`). Its newer line,
0.12, always pulls in `p256` and `crypto-bigint` 0.5.5, which has an
unfixed timing advisory (RUSTSEC-2026-0387), for a recipient type Crystal
does not use; Cargo builds a crate's whole dependency tree whatever part
of it is called, so staying on the crate meant staying on 0.11.

## Decision

- **The format stays age v1, unchanged.** Crystal implements the
  published format, never a construction of its own: same header,
  stanzas, MAC, payload key and STREAM. Files Crystal writes decrypt with
  age, rage or any age implementation; files they write to Crystal's
  recipient types decrypt with Crystal. Existing vaults open unchanged.
- **Ported, not invented.** `src/age_v1/` is ported from age 0.12.1 and
  age-core 0.12.0 (rage's library), trimmed to X25519 and scrypt
  recipients and whole buffers: no armor, plugins, SSH keys, i18n, async
  or streaming. The code is taken under age's MIT license
  (`LICENSES/age-MIT.txt`, "Copyright (c) 2019 Jack Grigg"); each derived
  file names its source file and version. Only the parsers are written by
  hand, with age's grammar and limits, because age's use nom and
  cookie-factory. Base64 and Bech32 are small own code that follows the
  RFC 4648 and BIP 173 rules age applies.
- **Primitives from RustCrypto and dalek**, as age uses them, now direct
  and exactly pinned: `chacha20poly1305` =0.10.1 (no `getrandom`
  feature), `scrypt` =0.11.0 (no default features), `x25519-dalek` =2.0.1
  (`static_secrets`, `zeroize`, `precomputed-tables`), `hkdf` =0.12.4,
  `hmac` =0.12.1, `sha2` =0.10.9 and `subtle` =2.6.1. All were already in
  the tree through age, at these versions.
- **Where age and the specification differ, Crystal follows the
  specification** (and Go age and the CCTV vectors):
  - the two legacy stanza encodings age 0.12.1 still accepts (no body
    line; a full last body line with no short line after it) are refused;
    age never writes them, so no Crystal file has them;
  - a Bech32 key string with non-zero padding bits is refused (age 0.12.1
    takes it, through bech32 0.11; age 0.11.5, Go age and BIP 173 do
    not); no age implementation writes one;
  - no random "grease" stanza is written (optional in the specification;
    Go age writes none); grease in existing files is read and skipped.
- **Limits for untrusted files:** a header of at most 64 KiB and 64
  stanzas, and the 32 MiB plaintext limit, checked before any work. The
  scrypt work factor accepted is still Crystal's policy
  ([0027](0027-scrypt-fixed.md)): log N 22 at most, and never more than
  half the locked-memory limit allows.
- **Secrets:** the file key lives in a heap box that is wiped on drop and
  has no Debug, Display or Clone; derived keys and every plaintext buffer
  are zeroizing and sized once. Errors are fixed variants with no key,
  passphrase or file bytes. No `unsafe`.
- **Randomness:** file keys, payload nonces, scrypt salts and X25519 keys
  come from `crypto::fill_random` (the `getrandom` syscall through rustix,
  flags 0), not from `rand`'s `OsRng`. Still in force from 0015: the
  seccomp filter allows `getrandom` (any length, flags 0) and never
  answers it with an errno. Until the `age` crate is removed (C3), age's
  own `getrandom` 0.2 probes once with a zero-length buffer and would fall
  back to `/dev/urandom`, which Landlock denies, if refused.
- **Still in force from 0002:** age is Crystal's only encryption format;
  no other primitives or custom KDF. Crystal never shells out to the
  `rage` or `age` binaries (that would need `execve` after unlock and push
  plaintext through pipes). A scrypt stanza is the only stanza of its
  file ([0004](0004-two-identity-files.md)). Hardware keys (YubiKey, TPM)
  were to come through age plugins in Phase 8; the plugin protocol is not
  part of `src/age_v1/`, so Phase 8 decides how, as a new record.
- **Steps.** C1: `src/age_v1/` next to the `age` crate, unused by the
  program, checked against it and the vectors. C2: `crypto.rs` and
  `vault/format.rs` switch to it; `age` becomes a dev-dependency for the
  cross-checks. C3: `age` is removed. All three come before Phase 4.
- **Verification:**
  - every C2SP/CCTV age vector for what Crystal implements (92 of 147;
    the armored and hybrid ones are skipped, as the CCTV README allows)
    gives its expected result, and their file keys re-encode the headers
    and re-encrypt the payloads byte for byte. The vectors are stored
    unchanged in `tests/corpus/age-cctv/` (`scripts/import-cctv.sh`,
    pinned commit); the compressed ones are inflated by `miniz_oxide`
    =0.8.9, a dev-dependency only;
  - cross-checks against the `age` crate in both directions, X25519 (one
    and three recipients) and scrypt, from 0 bytes to 32 MiB;
  - tests ported from age, and property tests (round trip; a flipped bit,
    truncation or extension fails; random bytes never panic) at a million
    cases in `scripts/long-tests.sh`.

## Alternatives

- **Keep the `age` crate (0.11):** 102 crates for two recipient types,
  build-time advisories, and no way to newer age code without
  `crypto-bigint` 0.5.5.
- **Move to `age` 0.12:** pulls in RUSTSEC-2026-0387 whatever Crystal
  calls; `cargo audit` fails.
- **A different format or own construction** (raw ChaCha20-Poly1305 with
  Argon2): loses the shared specification, the other implementations
  that can open Crystal's files, and the shared test vectors; custom
  constructions are where tools fail.
- **Shell out to `rage`:** needs `execve` after unlock and pipes for
  plaintext (as in 0002).
- **Write it from the specification alone:** more chances to differ from
  the implementations; porting age's code keeps its checks and comments.

## Consequences

- Crystal's dependency tree shrinks by about a hundred crates once C3
  removes `age`, along with its build-time advisories.
- Crystal now maintains about 1,200 lines of age code (3,000 with
  tests). Changes to the age specification are not picked up by a crate
  upgrade; the CCTV vectors and the cross-checks are what keep the code
  honest, and `scripts/import-cctv.sh` updates the vectors.
- Crystal's files stay readable by any age tool, which remains the
  recovery path of last resort.
- Post-quantum: the hybrid recipient no longer waits for rage; it can be
  ported into `src/age_v1/` from the specification once an ML-KEM
  implementation is approved ([0003](0003-no-post-quantum-yet.md)).
- Plugins (Phase 8) need a new decision.
