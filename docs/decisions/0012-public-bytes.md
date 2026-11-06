# 0012 Two plaintext exceptions: VERSION and .gen

- **Date:** 2026-11-06 (#7)
- **Status:** Accepted

## Context

The rule was "Crystal writes only ciphertext". Two files are plaintext by
design: `VERSION` (the format version, read before anything is
decrypted) and the `*.gen` generation counters in the state directory
(rollback detection, local only).

## Decision

`atomic_write` takes a `Payload`: either `Ciphertext` (constructible only
by the `crypto` module) or `PublicBytes` (constructible only by the
`VERSION` and `.gen` encoders, #10 and #11). Callers cannot pass arbitrary
bytes; the rule in CLAUDE.md now reads "only ciphertext, or the fixed
public formats `VERSION` and `*.gen`".

## Alternatives

- **Encrypt `VERSION` and the counters:** `VERSION` must be readable
  before unlock to pick the format; the counters must be checked against
  a decrypted generation, so encrypting them adds nothing.
- **A plain `&[u8]` writer:** nothing stops plaintext notes from reaching
  disk.

## Consequences

Plaintext on disk is limited to a format number and generation counts,
which reveal no note content.
