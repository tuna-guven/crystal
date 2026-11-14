# 0051 RustCrypto and dalek primitives moved to their current lines

- **Date:** 2026-11-14 (after Phase 5)
- **Status:** Accepted. Replaces the version list under "Primitives from
  RustCrypto and dalek" in [0042](0042-own-age-implementation.md); the
  rest of 0042 stays in force.

## Context

[0042](0042-own-age-implementation.md) pinned the primitives under
Crystal's own age v1 code at the versions the `age` crate used:
`chacha20poly1305` 0.10.1, `scrypt` 0.11.0, `x25519-dalek` 2.0.1, `hkdf`
0.12.4, `hmac` 0.12.1 and `sha2` 0.10.9. Each has a newer major release
on the current RustCrypto and dalek lines (`generic-array` replaced by
`hybrid-array`, new `aead`, `digest` and `cipher` traits), and RustCrypto's
maintenance has moved there. These crates are security critical, so what they
compute must stay exactly as it was.

## Decision

- **Exact pins, current lines:** `chacha20poly1305` =0.11.0, `scrypt`
  =0.12.0, `x25519-dalek` =3.0.0, `hkdf` =0.13.0, `hmac` =0.13.0 and
  `sha2` =0.11.1. `subtle` =2.6.1 and `zeroize` =1.9.1 stay. The features
  stay as before (no `getrandom`, no `password-hash`; `static_secrets`,
  `zeroize`, `precomputed-tables`), with one addition below.
- **`zeroize` on `chacha20poly1305`:** 0.10 always wiped the AEAD's copy
  of its key on drop; 0.11 does so only with this feature. It is turned
  on so that nothing stops being wiped.
- **The code moves to the new APIs, without new behaviour:** `AeadInOut`
  in place of the deprecated `AeadInPlace` (the tag is still checked in
  constant time before anything is decrypted, and the in-place buffers
  still never reallocate); the tag from a slice through `TryFrom`, which
  cannot fail on the 16 bytes split off; `KeyInit` for
  `Hmac::new_from_slice` (`verify_slice` stays the constant-time check);
  `scrypt::Params::new` without its length argument (the 32-byte output
  buffer sets it). For r = 8 scrypt refuses the same work factors as
  before: 0.12 dropped only the `log_n < 16 r` bound, which is 128 here
  and could never be reached.
- **Transitive crates, approved by the user:** `hybrid-array` 0.4.15
  replaces `generic-array` (with its build dependency `version_check`);
  `ctutils` 0.4.8 and `cmov` 0.5.4, RustCrypto's constant-time helpers,
  are used by `digest` and `universal-hash` in place of `subtle`;
  `opaque-debug` is gone; `rand_core` moves from 0.6 to 0.10 (a
  dependency of `x25519-dalek`; Crystal still takes no RNG from it).
  `Cargo.lock` goes from 78 crates to 75, since `chacha20`, `cpufeatures`
  and `rand_core` are now the versions `proptest` already brought.
- **Proof that nothing changed:** the C2SP/CCTV vectors and the stored
  files the `age` crate wrote ([0043](0043-age-checked-against-stored-files.md))
  pass unchanged, re-encoded byte for byte, in debug and release builds,
  with the property tests and the 32 MiB round trips.

## Alternatives

- **Stay on the old lines:** no new crates, but fixes stop reaching
  Crystal, and the old lines drift out of RustCrypto's maintenance.
- **Upgrade without the `zeroize` feature:** fewer lines in `Cargo.toml`,
  but the AEAD key would silently stop being wiped (zalloc wipes it on
  free only when it is on the heap; the cipher lives on the stack).
- **Enable `zeroize` on `hmac`/`sha2` too:** new wiping the old versions
  never did; left for its own decision, since this upgrade keeps
  behaviour as it was.

## Consequences

- Three new crates to audit (`hybrid-array` contains `unsafe`, as
  `generic-array` did; `cmov` uses inline assembly for its constant-time
  moves); three fewer in the lock file overall.
- The MSRV of these crates is 1.85, below the pinned toolchain (1.99.0).
- The next upgrade follows the same rule: exact pins, the CCTV vectors
  and the stored age files decide.
