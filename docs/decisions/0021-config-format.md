# 0021 Config format 1 and vault pinning

- **Date:** 2026-11-06 (#13)
- **Status:** Superseded by [0032](0032-fixed-locations.md) (no config file)

## Context

`config.age` maps vault names to directories on one device
([0005](0005-untrusted-config.md)). The design first said each entry's
`vault_id` would be "filled in at first unlock", but a normal session must
not write the config: after unlock the sandbox has no write access to
`~/.config/crystal`. `--add` also needs a passphrase on a device that has
no config yet.

## Decision

- **Format 1:** postcard `format | generation | lock_after | vaults`, each
  entry `name | path | vault_id | lock_after`, padded and encrypted to a
  scrypt passphrase (plugin recipients in Phase 8). Decode and encode
  apply the same rules: at most 256 vaults, valid and unique names,
  absolute clean UTF-8 paths, lock times of 1 to 1440 minutes.
- **Pinning:** `vault_id` is required and is pinned when a vault is
  registered (`--init`, `--add`), which both know it. Approved on
  2026-11-06.
- **Load and store:** load decrypts and then checks `config.gen`
  ([0019](0019-generation-counters.md)); store raises the generation,
  writes atomically, then updates `config.gen`.
- **`--add` order:** resolve the directory, run the vault path checks
  ([0020](0020-vault-path-checks.md)), and only then prompt. The config's
  passphrase also opens the vault's `identity.age`; on a device without a
  config, the new passphrase (entered twice) becomes the config's.
- **Paths must be UTF-8:** a non-UTF-8 vault directory is refused at
  `--add`.

## Alternatives

- **Fill in `vault_id` at first unlock:** needs config writes during a
  normal session, against the sandbox design.
- **Keep `vault_id` optional:** an unpinned entry would accept any vault
  at that path.
- **Store paths as raw bytes:** supports every Linux file name, but adds a
  second string type to the format for a rare case.
- **Prompt before checking the path:** a typo costs a passphrase entry,
  and a refused path would leave the user wondering whether anything was
  written.

## Consequences

- Only `--init`, `--add` and `--remove` write `config.age`, and each exits
  afterwards.
- A vault whose `identity.age` uses a different passphrase than this
  device's config cannot be added (it is refused as "wrong passphrase");
  re-encrypting `identity.age` for a new passphrase comes with key
  management in Phase 8.
- Every scrypt operation costs about a second, so `--add` on an existing
  config takes about three: decrypt the config, open the vault's
  identity, re-encrypt the config.
