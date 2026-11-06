# Backups

Crystal backs up automatically and fully offline by copying ciphertext only, so backups never need the key and never hold plaintext.

| Layer | Location | When it runs | Protects against |
| --- | --- | --- | --- |
| Local snapshots | `vault/.snapshots/` on the same disk | After every save | Accidental edits and deletes, bad sync merges |
| External drive | A drive you plug in, marked by an encrypted `crystal-backup.age` file bound to the `vault_id` | On unlock, on lock, and by a udev or systemd user trigger when the drive is mounted | Disk failure, theft, a wiped laptop |
| Key backup | `identity.backup.age`: the vault identity encrypted to an offline backup recipient (paper key or a second YubiKey) | At init and after every key rotation | Losing the passphrase or hardware key |

**Store format.** Each backup location holds `snapshots/<UTC timestamp>-<sha256 prefix>.age`, byte-for-byte copies of `vault.age`, plus a `SHA256SUMS` file. A text vault of a thousand notes is a few MB, so full copies are cheap; add delta storage only if the vault grows large.

**Headless mode.** `crystal backup --headless` copies `vault.age` when its hash is new and verifies the copy. It never unlocks or encrypts anything, so it needs no secrets and is safe to run from a udev rule when the drive appears; backups happen even when Crystal is closed. It runs under the same Landlock and seccomp policy, scoped to the vault and the backup drive.

**Append-only.** Crystal only adds snapshots. The single exception is retention pruning, which runs after a successful verify and never touches the newest 10 snapshots. Default retention: every snapshot for 7 days, one per day for 30 days, one per month for 12 months.

**Verification.** On each interactive unlock, Crystal decrypts the newest snapshot on every reachable backup and compares it with the live vault. `crystal backup verify --full` decrypts everything. A failed verify shows a warning on the status line and blocks pruning.

**Restore.** `crystal restore --from <path> --snapshot <timestamp>` writes the restored state as a new generation above the current one, so rollback detection stays valid. Restores are always explicit, never automatic, and refuse snapshots whose `vault_id` does not match.

**Operational rules.** Unplug the drive between runs, since a mounted drive is reachable by anything running as your user. Put LUKS on the drive to also hide note count and snapshot history. Keep a second drive off-site if you can. Test a full restore to a scratch directory once a month.
