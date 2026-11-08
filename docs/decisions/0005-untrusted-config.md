# 0005 Encrypted per-device config, treated as untrusted

- **Date:** 2026-11-06
- **Status:** Superseded by [0032](0032-fixed-locations.md) (no config file)

## Context

Vaults are opened by name, so something must map names to directories.
Paths differ between machines, and the map reveals where vaults live.
Under a public-key or plugin recipient, anyone who knows the recipient
can write a valid age file, so encryption alone does not prove who wrote
the config.

## Decision

`~/.config/crystal/config.age` is local to each device, padded and
encrypted to one unlock method. It holds nothing that loosens security
(no sandbox switches, executable paths or environment). Every path in it
is checked as if an attacker wrote it: absolute, no `..`, opened one
component at a time with `O_NOFOLLOW`, every parent owned by root or you
and not group- or world-writable, the vault itself yours, mode 0700, with
a `VERSION` file, and not `$HOME` or near the config or state
directories. Only `--init`, `--add` and `--remove` write it. See
[Storage, Config file](../design/storage.md#config-file).

## Alternatives

- **Vault path on the command line:** paths in argv and shell history,
  and a typo opens the wrong directory.
- **A plaintext config:** leaks vault names and locations.
- **Trusting the config because it decrypts:** fails under public-key
  recipients.

## Consequences

The config can point Crystal at a vault, but cannot widen the sandbox
beyond one checked, private directory. Losing it loses nothing: `--add`
registers an existing vault again. A generation counter (`config.gen`)
rejects an older config.
