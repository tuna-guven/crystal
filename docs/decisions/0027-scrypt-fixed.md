# 0027 scrypt: one fixed work factor, log N 18

- **Date:** 2026-11-06, fixed at log N 18 on 2026-11-07 (Phase 2 review)
- **Status:** Accepted

## Context

The passphrase protects `identity.age` with scrypt. A high work factor
slows guessing; tests need it low to run fast. A planted file could ask
for an enormous work factor and stall or crash unlock: scrypt needs
2^(log N + 10) bytes of locked memory ([0026](0026-memlock-floor-512.md)),
and an allocation that does not fit aborts Crystal.

Earlier, Crystal let age time its one-second factor on each run, capped
to half the locked-memory limit. The factor then depended on the device
that wrote the file: a fast desktop with a 2 GiB limit picked log N 19 or
20, and a device at the floor, which fits only 18, would refuse that
`identity.age`, so a synced vault could not be opened there. age's
decryption allowance (the timed factor plus 4) meant a slow device could
also refuse a file written at 18 by a fast one. The first release build
even aborted in `crystal init`, because age's choice on a fast machine
needed the whole budget.

## Decision

- **Encryption:** passphrase files are written at log N 18
  (`crypto::ENCRYPT_LOG_N`), 256 MiB, on every device. 18 is the largest
  factor whose memory fits in half the 512 MiB floor, which a test checks.
  Nothing is timed. The recovery code's file (`identity.backup.age`) uses
  the same factor ([0029](0029-recovery-code.md)).
- **Decryption:** accept up to log N 22, 16 times Crystal's own cost
  (age's allowance), and never more than fits in half this device's
  limit. A file above that is refused as `WorkFactorTooHigh` before any
  scrypt work, with a message pointing to the setup guide. An unlimited
  limit is still capped at 22, so a planted file cannot ask for
  terabytes.
- **Tests:** unit tests use log N 10. Integration tests run the real
  binary at 18; debug and test builds optimize only the scrypt crates
  (`[profile.dev.package.*]` in Cargo.toml), which keeps the suite fast.
  Release and debug binaries have no way to lower the factor.

## Alternatives

- **Adaptive (timed, capped by memory):** a faster or larger machine
  writes files that smaller devices refuse.
- **Adaptive minimum (from RAM, the limit or timing):** see
  [0026](0026-memlock-floor-512.md); the floor must hold on every device.
- **Accept anything that fits in memory:** with an unlimited limit that
  is log N 30, a 1 TiB allocation from a planted file.
- **Run scrypt before `mlockall`:** its memory holds passphrase-derived
  state, which belongs in locked memory.
- **A stronger factor by explicit choice** (`--scrypt-memory`): an open
  question in [Storage](../design/storage.md); every device that opens
  the vault would need the matching limit.

## Consequences

- Same cost everywhere: about 0.5 s per passphrase file in a release
  build on a fast machine; slower on old hardware, never weaker.
- Guessing resistance comes from the 20-character passphrase minimum
  ([0028](0028-passphrase-minimum.md)) plus this fixed factor, not from
  the machine that created the vault.
- `cargo test --release` at the 512 MiB floor is the test that catches a
  factor that does not fit (docs/development/testing.md).
