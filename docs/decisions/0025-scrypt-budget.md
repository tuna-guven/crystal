# 0025 scrypt within the locked-memory budget

- **Date:** 2026-11-06
- **Status:** Accepted; supersedes [0014](0014-scrypt-work-factor.md)

## Context

Crystal locks all its memory (`mlockall` with `MCL_FUTURE`), so every
allocation counts against `RLIMIT_MEMLOCK`, and one that does not fit
fails and aborts the process. scrypt needs 2^(log N + 10) bytes. age picks
log N by timing (about one second), which an optimized build on a fast
machine puts at 18: 256 MiB, the whole of Crystal's 256 MiB minimum
budget. The first release build aborted in `crystal init`. Debug builds
run scrypt slowly, picked about 15, and hid the problem. On decryption,
age accepts files up to 16 times its own target, so a planted config could
ask for gigabytes and crash Crystal instead of being refused.

## Decision

Per run, from the memlock hard limit and a timing measured as age measures
it:

- **Encryption:** age's one-second factor, but at most the largest log N
  whose memory fits in half the limit. The other half stays for the vault
  and everything else.
- **Decryption:** age's allowance (one-second factor plus 4) with the same
  cap. A file above it is refused as `WorkFactorTooHigh` before any scrypt
  work, with a message pointing to the setup guide.
- **Tests:** test builds still use log N 10 (cap 14); release binaries
  have no way to lower the factor.

At the 256 MiB floor this gives log N 17 (128 MiB, about half a second
here). With the limit at 1 GiB or more, age's default runs unchanged; a
larger limit, such as 16 GiB, is fine and changes nothing else.

## Alternatives

- **A fixed log N:** too weak on fast machines with memory to spare, too
  slow on old ones.
- **Raise the minimum to 512 MiB:** every user would need a bigger limit,
  and a faster machine would need more again.
- **Run scrypt before `mlockall`:** its memory holds passphrase-derived
  state, which belongs in locked memory.
- **Go beyond age's default when memory allows:** possible later, as an
  explicit choice; each step doubles both memory and unlock time.

## Consequences

- At the minimum limit, passphrase files are about half as costly to guess
  as age's default on a fast machine. The setup guide recommends 1 GiB for
  the full default.
- A file made where more memory was allowed can be refused on a device
  with a smaller limit; the message says to raise it.
- `cargo test --release` at the floor is the test that catches a factor
  that does not fit (docs/development/testing.md).
