# 0014 scrypt work factor: age's default, capped on decrypt

- **Date:** 2026-11-06 (#8)
- **Status:** Accepted

## Context

The passphrase protects `identity.age` and `config.age` with scrypt. A
high work factor slows guessing; tests need it low to run fast. A planted
file could ask for an enormous work factor and stall unlock for hours.

## Decision

- **Encryption:** age's own choice, about one second on the machine
  running `--init`. Only `#[cfg(test)]` lowers it (log N = 10); release
  and debug binaries have no way to change it.
- **Decryption:** age refuses a file whose work factor is more than 16
  times its own one-second target on this machine; Crystal reports that
  as `WorkFactorTooHigh`, before any scrypt work.

## Alternatives

- **A fixed work factor:** too slow on old hardware or too weak on new.
- **A runtime or debug-build knob:** could end up in a shipped binary.

## Consequences

A vault created on a fast machine may be slow to unlock on a much slower
one, and refused if it is over 16 times slower. Integration tests that
run the real binary through `--init` will take about a second per
passphrase operation.
