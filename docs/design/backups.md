# Backups

Crystal has no backup code. Back up `~/.local/share/crystal` with a dedicated tool, such as restic or borg ([decision 0036](../decisions/0036-backups-external.md)). They already do deduplication, encrypted repositories, retention and verification, and do them well; Crystal stays small.

Deleting a note or a vault is final ([decision 0032](../decisions/0032-fixed-locations.md)), so a backup is part of setting up Crystal, not an extra.

## What to back up

| Path | Holds | Needed for |
| --- | --- | --- |
| `~/.local/share/crystal/NAME/` | `VERSION`, `identity.age`, `identity.backup.age`, `vault.age`: ciphertext and a format number | Everything |
| The recovery code, on paper | Opens `identity.backup.age` | A forgotten passphrase |

Do not back up `~/.local/state/crystal`: its pin and rollback counters belong to one device.

## Why Crystal's files suit a backup tool

- **Ciphertext only.** The tool never sees plaintext; its own repository password is a second layer.
- **Atomic writes.** A backup taken during a save holds the old or the new `vault.age`, never half of one; a leftover temp file is swept on the next save.
- **One fixed directory**, so one backup path covers every vault.

**Deduplication barely applies, by design.** Each save makes `vault.age` an entirely new ciphertext, so the tool stores each changed version in full. Making it deduplicate would mean encrypting in fixed chunks or deterministically, which would show which part of the vault, and roughly which note, changed: what the single padded blob hides. For text the cost is small: a 5 MB vault backed up daily is under 2 GB a year before pruning.

## Restoring

1. Restore `~/.local/share/crystal/NAME` with your tool (into an empty place, or over the current vault).
2. Run `crystal NAME`. If the restored vault is older than the newest version this device has seen, Crystal stops and explains that it is older: restored from a backup, or put back by someone else. Type `restore` to accept it, and only if you restored it yourself. The rollback counter then starts again from the restored version ([Storage, Rollback](storage.md#rollback)).
3. On a new device, there is no counter yet: the first unlock accepts the vault and pins it.

If the passphrase is lost too, `crystal recover NAME` with the recovery code sets a new one. `crystal check-code NAME` checks the code on paper against the vault without changing anything; run it once after `crystal init`.

## A restic setup

The setup guide walks through it ([Setup](../guide/setup.md)); in short:

```sh
restic init --repo /run/media/you/backup/crystal
restic --repo /run/media/you/backup/crystal backup ~/.local/share/crystal
restic --repo /run/media/you/backup/crystal forget --keep-daily 7 --keep-weekly 5 --keep-monthly 12 --prune
restic --repo /run/media/you/backup/crystal check
```

Run the backup from a systemd user timer, keep one repository off-site, and test a restore into a scratch directory now and then.
