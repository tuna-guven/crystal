# 0027 scrypt: one fixed work factor, log N 18

- **Date:** 2026-11-07 (Phase 2 review)
- **Status:** Accepted; supersedes [0025](0025-scrypt-budget.md)

## Context

[0025](0025-scrypt-budget.md) encrypted passphrase files at age's
one-second factor, measured on each run, capped at what fits in half the
`RLIMIT_MEMLOCK` limit. So the factor depended on the device that wrote
the file: a fast desktop with a 2 GiB limit picked log N 19 or 20.

`identity.age` is encrypted to the passphrase and sits in the vault
directory, so it travels with the vault to every device. A device at the
floor can only fit log N 18 ([0026](0026-memlock-floor-512.md)) and
would refuse an `identity.age` written at 19: the vault would be
unreadable there. A higher factor on `config.age` alone protects nothing,
because the same passphrase also protects `identity.age`, and an attacker
guesses against the cheaper file. The decryption cap had a second flaw:
age's allowance of the timed factor plus 4 meant a slow device could
refuse a file written at 18 by a fast one.

## Decision

- **Encryption:** both passphrase files (`config.age` and `identity.age`)
  are written at log N 18 (`crypto::ENCRYPT_LOG_N`), on every device.
  18 is the largest factor whose memory, 256 MiB, fits in half the
  512 MiB floor, which a test checks. Nothing is timed any more.
- **Decryption:** accept up to log N 22, 16 times Crystal's own cost
  (age's allowance), and never more than fits in half this device's
  limit. A file above that is refused as `WorkFactorTooHigh` before any
  scrypt work, with a message pointing to the setup guide. Files written
  under 0025 at log N 19 to 22 still open where memory allows; the next
  passphrase change (Phase 8) rewrites them at 18. An unlimited limit is
  still capped at 22, so a planted file cannot ask for terabytes.
- **Tests:** unit tests still use log N 10. Integration tests run the
  real binary at 18; debug and test builds optimize only the scrypt
  crates (`[profile.dev.package.*]` in Cargo.toml), which keeps the suite
  fast. The binary has no way to lower the factor.

## Alternatives

- **Adaptive, as in 0025:** a faster or larger machine writes files that
  smaller devices refuse, and gains nothing while another file under the
  same passphrase stays at the lower factor.
- **Adaptive minimum (from RAM, the limit or timing):** see
  [0026](0026-memlock-floor-512.md); the floor must hold on every device.
- **Accept anything that fits in memory:** with an unlimited limit that
  is log N 30, a 1 TiB allocation from a planted file.
- **A stronger factor by explicit choice** (`--scrypt-memory`): still an
  open question in [Storage](../design/storage.md), for Phase 8; every
  device that opens the vault would need the matching limit.

## Consequences

- Same cost everywhere: about 0.5 s per file in a release build on a
  fast machine, so about 1 s per unlock (two files); slower on old
  hardware, never weaker.
- Guessing resistance comes from the 20-character passphrase minimum plus
  this fixed factor, not from the machine that created the vault.
- No more `clock_gettime` for timing scrypt.
