# 0013 Vault identity in a Box, not SecretBox

- **Date:** 2026-11-06 (#8)
- **Status:** Accepted

## Context

The plan (#8) was `VaultIdentity(SecretBox<age::x25519::Identity>)`.
`secrecy::SecretBox<T>` requires `T: Zeroize`, and age's identity type
does not implement it, so that does not compile. Its key is an
x25519-dalek `StaticSecret`, which zeroizes itself on drop when
x25519-dalek's `zeroize` feature is on (it is, in our dependency tree).

## Decision

`VaultIdentity(Box<age::x25519::Identity>)`: heap only, wiped on drop by
the key type itself, no Debug, Display, Clone or Serialize (checked at
compile time). The secret is serialized only inside `crypto`, straight
into an age encryption call (`seal_with_passphrase`,
`seal_to_recipients`), and parsed only from a decrypted, zeroizing
buffer (`open_with_passphrase`).

## Alternatives

- **Store the identity's `AGE-SECRET-KEY-1…` string in a `SecretString`
  and parse it per use:** age's own string conversion leaves a bech32
  intermediate it does not wipe, on every use.
- **A wrapper with an empty `Zeroize` impl:** satisfies `SecretBox` while
  doing nothing; misleading.

## Consequences

Wiping relies on x25519-dalek's `zeroize` feature staying on; a
dependency update that drops it would be a regression to catch in
review. The Phase 5 zeroizing allocator is the backstop.
