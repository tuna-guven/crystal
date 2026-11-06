# 0004 Two identity files

- **Date:** 2026-11-06
- **Status:** Accepted

## Context

The vault is encrypted to a random X25519 vault identity. That identity
must be unlockable daily (passphrase or hardware key) and recoverable if
the passphrase or key is lost (an offline backup key). age accepts a
scrypt stanza only as the sole recipient of a file.

## Decision

`crystal --init` stores the vault identity twice: `identity.age`,
encrypted to the passphrase alone or to hardware-key recipients, and
`identity.backup.age`, encrypted to the offline backup recipient(s). See
[Storage, Key design](../design/storage.md#key-design).

## Alternatives

- **One identity file with passphrase and backup recipients:** age forbids
  mixing scrypt with other recipients.
- **Encrypt `vault.age` to the backup key too:** every save would need the
  backup recipient, and rotating it would rewrite the vault.

## Consequences

- Changing the passphrase or adding a hardware key rewrites only
  `identity.age`; rotating the backup key rewrites only
  `identity.backup.age`.
- Two files to keep in sync when the identity is created; both are
  ciphertext and safe to sync.
