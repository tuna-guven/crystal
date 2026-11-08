# 0032 Fixed locations, no config file, one `rm`

- **Date:** 2026-11-08 (Phase 3 redesign)
- **Status:** Accepted. Supersedes [0005](0005-untrusted-config.md) and
  [0021](0021-config-format.md); the path walk of
  [0020](0020-vault-path-checks.md) for configured paths; `--at` in
  [0022](0022-init.md); the config steps of [0023](0023-unlock.md);
  `add` and `remove` in [0024](0024-subcommands.md); `config.gen` in
  [0019](0019-generation-counters.md); the config and DIR parts of
  [0029](0029-recovery-code.md)'s `recover`.

## Context

Crystal is meant to be minimal and auditable. Vaults could live anywhere
(`--at DIR`, `crystal add NAME DIR`), so an encrypted per-device config
(`config.age`) mapped names to paths, and every path in it was treated as
attacker-written: a walk from `/` with owner and mode checks on each
component, refusing `$HOME`, `~/.ssh` and the config and state
directories. That is about 1,200 lines (`config.rs`, most of
`vault_dir.rs`, `add`, `remove`, the config steps of `init` and
`recover`) and the largest untrusted-input surface outside the vault
format. It also tied all vaults on a device to one passphrase, and made
`recover` ask whether to replace a config whose passphrase was lost.

## Decision

- **Vaults live in `~/.local/share/crystal/NAME`**, with no option to put
  them elsewhere (`XDG_DATA_HOME` is not read). `crystal` lists vaults by
  reading that directory. It is reached from `HOME` one component at a
  time with `O_NOFOLLOW`; `.local` and `share` must be yours and not
  group- or world-writable; `crystal` and each vault must be yours, mode
  0700. `crystal init` creates `crystal` (0700) if missing.
- **No `config.age`.** `~/.config/crystal` is the fixed place for settings
  later: a small plain-text file you write and Crystal only reads. It is
  not created until a setting needs it.
- **One passphrase per vault**, the one that seals its `identity.age`.
  Using the same passphrase for several vaults is your choice.
- **Pin and counter in the state directory.** `~/.local/state/crystal/
  NAME.pin` holds the vault's `vault_id` (64 lowercase hex digits and a
  newline), set at `crystal init` or at the first unlock on a device;
  unlock refuses a vault whose `vault_id` differs. The generation counter
  stays `<vault_id>.gen`. Both are fixed public formats
  ([0012](0012-public-bytes.md)), written by the atomic writer.
- **Commands:** `crystal init NAME`, `crystal NAME`, `crystal recover
  NAME` (code, then a new passphrase twice; it rewrites `identity.age`
  only), `crystal check-code NAME` (checks the recovery code against
  `identity.backup.age` and `identity.age`, changes nothing; formerly
  planned as `verify-backup`), `crystal rm NAME`, `crystal help`. `add`,
  `remove`, `--at` and the DIR argument go away. Reserved names: `init`,
  `rm`, `recover`, `check-code`, `help`.
- **`crystal rm NAME` deletes a vault for good**, after the name is typed
  again. The prompt says that only a backup can bring it back. It removes
  `VERSION`, `identity.age`, `identity.backup.age`, `vault.age`, Syncthing
  conflict copies (`vault.sync-conflict-*.age`), leftover atomic-writer
  temp files, then the directory, and the vault's `.pin` and `.gen`. Any
  other file in the directory is left alone, and so is the directory; the
  message says so. There is no trash: one verb, one meaning. `rm` in the
  shell deletes a note the same way ([0033](0033-shell.md)).
- **Deleting files** is a new kind of disk change: `crystal rm` (and
  `init`'s undo, which already did) may `unlinkat` Crystal's own files
  and `rmdir` a vault directory, before the sandbox and then exit.
  `CLAUDE.md` names this exception.

## Alternatives

- **Keep a configurable location:** keeps the 1,200 lines and the path
  attack surface for a convenience a symlink-free sync setup does not
  need: point Syncthing or restic at `~/.local/share/crystal/NAME`.
- **Keep `config.age` for settings:** a passphrase-protected file for a
  few non-secret numbers, plus a second scrypt run per unlock.
- **A trash for deleted vaults:** two more commands (`undel`, `purge`)
  and a second meaning of "deleted"; backups do this job.
- **No pin:** a sync attacker could swap one of your vaults for another
  of yours that shares the passphrase, and it would open under the wrong
  name. The pin costs one small file.

## Consequences

- About 1,200 lines and their tests are removed (L1 in the roadmap); the
  vault path is known at compile time apart from `HOME` and NAME.
- Sandboxing gets simpler: Landlock grants one known vault directory; the
  SELinux policy labels `~/.local/share/crystal` statically, with no
  `semanage` rule per vault; the bubblewrap launcher always knows the
  path.
- Unlock runs scrypt once instead of twice (no config to decrypt).
- A vault synced from another device is opened by copying or syncing it
  into `~/.local/share/crystal/NAME`; its first unlock pins it.
- The pin and counter are plaintext; a process running as you can change
  them (A5), as before ([0019](0019-generation-counters.md)).
