# 0002 age only, linked as a library

- **Date:** 2026-10-11
- **Status:** Accepted

## Context

Crystal needs authenticated encryption of the vault, passphrase-based key
wrapping and, later, hardware keys. Custom constructions are where
security tools fail.

## Decision

Use age, through the `age` crate (the library of the rage project), for
all encryption: X25519 recipients, scrypt passphrases and, from Phase 8,
plugins for YubiKey or TPM. No other primitives, no custom KDF. Never
shell out to the `rage` binary. See
[Dependencies](../design/dependencies.md).

## Alternatives

- **Raw primitives (ChaCha20-Poly1305, Argon2):** more code to get wrong
  and to review.
- **The `rage` binary through a pipe:** needs `execve` after unlock and
  pushes plaintext through pipes.
- **GPG:** large attack surface, complex formats.

## Consequences

- One well-reviewed format; files can be inspected or recovered with any
  age implementation.
- Crystal inherits age's limits: no hybrid post-quantum recipient in rage
  yet ([0003](0003-no-post-quantum-yet.md)), and a scrypt stanza must be
  the only recipient of a file ([0004](0004-two-identity-files.md)).
