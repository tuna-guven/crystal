# 0043 Crystal's age code is checked against stored age files, not the age crate

- **Date:** 2026-11-09 (step C3, before Phase 4)
- **Status:** Accepted. Replaces the part of
  [0042](0042-own-age-implementation.md) that verified Crystal's age code
  with cross-checks against the `age` crate; the rest of 0042 stands.

## Context

Decision 0042 replaced the `age` crate with Crystal's own implementation
of the age v1 format (`src/age_v1/`), ported from age 0.12.1. In steps C1
and C2 the crate stayed as a dev-dependency to cross-check that code in
both directions, and the integration tests used it to read the real
binary's files. Keeping it in the tree, even for tests, kept 82 crates in
`Cargo.lock`, two advisory exceptions (RUSTSEC-2026-0374, RUSTSEC-2026-0173)
and nine duplicate-version exceptions in `deny.toml`.

What those cross-checks showed has to keep being shown: that Crystal opens
what age writes, and that age opens what Crystal writes. The format stays
age v1, unchanged; Crystal does not roll its own crypto.

## Decision

- **No `age` crate**, as a dependency or a dev-dependency.
- **Files age wrote, stored once.** Before the crate was removed it wrote
  `tests/corpus/age-rage/` (age 0.11.5, what Crystal linked before C2):
  X25519 files to one and to three keys, each with age's grease stanza,
  at 0, 1, 64 KiB − 1, 64 KiB, 64 KiB + 1 and 3 × 64 KiB; scrypt files as
  old Crystal wrote `identity.age` and `identity.backup.age`, one with a
  non-ASCII passphrase, and two at the test work-factor cap and one above
  it. Their keys, passphrases and plaintext SHA-256 are stored with them
  (`keys.txt`, `MANIFEST`), and `SOURCE` holds the generator. They are
  test data, never regenerated; 844 KiB.
- **Reading:** every stored file opens with each of its keys or its
  passphrase and gives the recorded plaintext; a stranger's key, a wrong
  passphrase and a work factor over the cap are refused
  (`age_v1::rage`, and the tests in `x25519.rs`, `scrypt.rs`, `header.rs`
  and `crypto.rs` that named the age crate).
- **Writing:** under the file key Crystal unwraps, each stored file's
  header (stanzas, grease and MAC) and payload re-encode to age's bytes
  exactly. Together with the C2SP/CCTV re-encoding (0042), this shows
  that what Crystal writes is what age writes. `crypto.rs` also checks
  the layout of what Crystal writes as text.
- **Integration tests use Crystal's own commands** on the real binary's
  files: `check-code` (the backup and `identity.age` hold the same key),
  `recover` and unlock (the backup's key opens `vault.age`), and a work
  factor changed in the header text (refused before any scrypt work).
- **Crystal's own round trips** keep the sizes and the 32 MiB case the
  cross-checks covered.
- **No interoperability script in the repository.** A one-off check with
  Go age v1.3.2 (2026-11-09) passed both ways: Go age opened a vault the
  real binary wrote, and the binary unlocked a vault Go age wrote. It is
  recorded in the testing chapter, not repeated in CI.

## Alternatives

- **Keep the `age` crate as a dev-dependency:** 82 more crates in the
  lock file and their advisory exceptions, for tests only.
- **A script that runs an installed `age` or `rage`:** the only check by a
  different implementation, but outside CI, needs software Crystal does
  not otherwise use, and Go age reads passphrases only from a terminal.
  Not now.
- **Compile `src/age_v1/` into the integration tests:** they would check
  Crystal's files with Crystal's own code, which says nothing more.

## Consequences

- `Cargo.lock` goes from 160 to 78 crates; `deny.toml` has no advisory
  exception and one duplicate-version exception (`syn`, build time).
- Compatibility with age now rests on stored files and the CCTV vectors,
  which do not change, instead of a crate that could. A new age feature
  Crystal wants is a new decision with new vectors, as 0042 already says.
- The check that age reads a 32 MiB file Crystal wrote is gone; the
  STREAM code that writes it is checked byte for byte at 3 × 64 KiB and
  round-trips at 32 MiB.
