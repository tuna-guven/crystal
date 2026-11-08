# 0036 Backups with dedicated tools; restoring an older vault

- **Date:** 2026-11-08 (Phase 3 redesign)
- **Status:** Accepted. Replaces the roadmap's Phase 7 (Crystal's own
  backup system) and supersedes "never lowered" in
  [0019](0019-generation-counters.md) for an explicit restore.

## Context

The roadmap planned Crystal's own backups: local snapshots after every
save, an external drive marked by an encrypted file, a headless mode
started by udev or systemd, retention pruning, verification and restore.
restic and borg already do all of this, with deduplication, encrypted
repositories, retention policies and verification, and they are widely
used and reviewed. Crystal is meant to stay minimal.

Every save makes `vault.age` an entirely new ciphertext (a new age file
key and nonces), so a deduplicating tool stores each changed version in
full. Encrypting in fixed chunks or deterministically would let it
deduplicate, but would show which part of the vault, and roughly which
note, changed, which the single padded blob hides
([0001](0001-single-vault-blob.md)). For text notes the cost is small: a
5 MB vault backed up daily is under 2 GB a year before pruning.

Rollback detection ([0019](0019-generation-counters.md)) refuses a
`vault.age` older than the newest one this device has seen. That is right
against an attacker who puts back an old copy, and wrong for a restore
from a backup.

## Decision

- **Crystal has no backup code.** Back up `~/.local/share/crystal` (all
  vaults: ciphertext, `identity.age`, `identity.backup.age`, `VERSION`)
  with restic, borg or any file-level tool. The setup guide has a restic
  chapter: repository with its own password, a timer, `forget --prune`,
  and a monthly test restore. The recovery code on paper plus one backup
  restores everything.
- **What makes Crystal backup-friendly**, already true: only ciphertext
  in the vault directory; the atomic writer, so a backup taken during a
  save holds the old or the new `vault.age`, never half of one; one fixed
  directory ([0032](0032-fixed-locations.md)).
- **Restore acceptance at unlock:** when the decrypted vault's generation
  is lower than this device's counter, unlock stops and explains that the
  vault is older than the newest version this device has seen, either
  restored from a backup or put back by someone else. Typing `restore`
  accepts it: the counter is set to the vault's generation, and the next
  save continues from there. Anything else leaves both untouched and
  exits.
- **`crystal check-code NAME`** checks the recovery code against
  `identity.backup.age` ([0032](0032-fixed-locations.md)); it is not a
  backup feature, and it stays.
- The `backup` module and `backup::store` are removed from the code and
  from the write rule in `CLAUDE.md`.

## Alternatives

- **Crystal's own snapshots and drive backups (the old Phase 7):** a
  large surface (drive marker, udev, headless mode, retention) that
  duplicates restic and borg.
- **Deduplication-friendly encryption:** leaks which notes change.
- **Refusing older vaults with no way to accept them:** a restore would
  need deleting the state file by hand.

## Consequences

- Phase 7 is gone from the roadmap; the sandbox needs no backup
  directory and no `MAKE_DIR` for snapshots.
- A sync attacker who puts back an old `vault.age` now gets a prompt
  instead of a plain refusal; the prompt text says that if you did not
  restore anything, you must not type `restore`. The counter is only
  ever lowered by that word.
- Deleting a note or a vault is final except through these backups
  ([0032](0032-fixed-locations.md), [0033](0033-shell.md)), so the guide
  presents a backup tool as part of setup, not an extra.
