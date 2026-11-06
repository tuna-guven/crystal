# 0003 X25519 vault recipient until rage has hybrid post-quantum

- **Date:** 2026-11-06 (#4)
- **Status:** Accepted

## Context

Synced ciphertext can be stored today and decrypted later by a quantum
computer ("harvest now, decrypt later"). Go age 1.3 has a native hybrid
ML-KEM-768 + X25519 recipient; rage does not.

## Decision

Pin `age` 0.11.5 and keep the vault recipient X25519. Revisit when rage
ships a native hybrid identity: upgrade `age` and re-encrypt the vault to
the new recipient on the next save.

## Alternatives

- **age 0.12's `tagpq` (`mlkem768p256tag`):** encryption-only; decrypting
  needs a hardware tag through a plugin, so it cannot be the vault's own
  recipient. It also pulls in `p256` and `crypto-bigint` 0.5, which has a
  timing advisory (RUSTSEC-2026-0387).
- **Third-party post-quantum crates:** unaudited.
- **Post-quantum through a plugin:** notes are encrypted on every save,
  and plugins need `execve`, which the sandbox denies after unlock.

## Consequences

The vault is not protected against a future quantum attacker who holds a
copy of today's ciphertext. Migration is one re-encryption once rage
supports it. Recorded in [Storage, Open questions](../design/storage.md#open-questions).
