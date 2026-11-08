# 0019 Generation counters for rollback detection

- **Date:** 2026-11-06 (#11)
- **Status:** Accepted; `config.gen` is superseded by
  [0032](0032-fixed-locations.md) (no config), and "never lowered" by
  [0036](0036-backups-external.md) (an explicit `restore`)

## Context

age authenticates `vault.age` and `config.age`, but an old copy is just
as authentic as the newest. A storage or sync attacker (A1) could put back
an older vault, silently undoing edits, or an older config, undoing a
removed vault or a changed passphrase. Detecting that needs state the
attacker does not control, which is local to the device.

## Decision

- Each encrypted file carries a `generation`, +1 per write. The state
  directory (`~/.local/state/crystal`, mode 0700, never synced) keeps the
  last one seen: `<vault_id hex>.gen` per vault and `config.gen`.
- **Format:** plaintext, ASCII decimal plus one newline, one valid
  spelling, written through the atomic writer as `PublicBytes`
  ([0012](0012-public-bytes.md)); read from a regular file only, a few
  bytes at most.
- **Open:** lower than the counter is refused with a message naming both
  generations; equal or higher is accepted and stored. No counter is the
  first use on this device and is reported to the user.
- **Save:** the counter is written after the rename commits, so a crash
  can only leave it behind the file, never ahead.
- **Fail closed:** a malformed counter is refused and never reset.

## Alternatives

- **Encrypt the counters:** the key is only available after unlock, and a
  same-user attacker who can write the file can also delete it, which
  resets detection either way; encryption would add complexity without
  stopping A5.
- **Keep the counter inside the vault only:** an attacker who replaces
  the vault replaces the counter with it.
- **Reset a corrupt counter to the vault's value:** an attacker could
  corrupt it on purpose to make the next rollback pass.
- **Write the counter before the vault rename:** a crash would leave the
  counter ahead of the file, and the vault would then be refused as a
  rollback of itself.

## Consequences

- Rollback is detected relative to this device only. A new device, or a
  lost state directory, trusts the first generation it sees.
- A process running as you (A5, "Partial" in the threat model) can lower
  or delete a counter and then roll the vault back. This was accepted when
  the issue was approved: such a process can already log the passphrase.
- Two Crystal instances saving the same vault can make a save report a
  rollback (the other got ahead); the counter is never lowered.
