# Summary

[Introduction](introduction.md)

# Design

- [Overview and goals](design/overview.md)
- [Threat model](design/threat-model.md)
- [Architecture](design/architecture.md)
- [Storage: layout, formats and keys](design/storage.md)
- [Startup order and process hardening](design/startup.md)
- [Plaintext leakage prevention](design/leakage.md)
- [Sandboxing](design/sandboxing.md)
- [Dependencies](design/dependencies.md)
- [Backups](design/backups.md)

# User guide

- [Setup](guide/setup.md)
- [Installation](guide/installation.md)

# Reference

- [Syscalls](reference/syscalls.md)

# Decisions

- [Decision log](decisions/README.md)
  - [0001 One encrypted vault blob](decisions/0001-single-vault-blob.md)
  - [0002 age only, linked as a library](decisions/0002-age-library-only.md)
  - [0003 X25519 vault recipient until rage has hybrid post-quantum](decisions/0003-no-post-quantum-yet.md)
  - [0004 Two identity files](decisions/0004-two-identity-files.md)
  - [0005 Encrypted per-device config, treated as untrusted](decisions/0005-untrusted-config.md)
  - [0006 Per-user install in ~/.local/bin](decisions/0006-per-user-install.md)
  - [0007 Fail closed on inherited file descriptors](decisions/0007-refuse-inherited-fds.md)
  - [0008 Locked memory floor of 256 MiB](decisions/0008-memlock-floor.md)
  - [0009 Signals through self-pipes, no threads](decisions/0009-signals-self-pipe.md)
  - [0010 Landlock ABI 9 required, no fallback](decisions/0010-landlock-abi-9.md)
  - [0011 Atomic writer: same-directory temp, renameat2, crash sweep](decisions/0011-atomic-writer.md)
  - [0012 Two plaintext exceptions: VERSION and .gen](decisions/0012-public-bytes.md)
  - [0013 Vault identity in a Box, not SecretBox](decisions/0013-identity-box.md)
  - [0014 scrypt work factor: age's default, capped on decrypt](decisions/0014-scrypt-work-factor.md)
  - [0015 age's random source must be allowed by seccomp](decisions/0015-age-getrandom.md)
  - [0016 Passphrase prompt without echo or reallocation](decisions/0016-passphrase-prompt.md)
  - [0017 Documentation as an mdBook from docs/](decisions/0017-mdbook-docs.md)
  - [0018 Vault format 1](decisions/0018-vault-format-1.md)
  - [0019 Generation counters for rollback detection](decisions/0019-generation-counters.md)
  - [0020 Vault paths: walk from /, compare by inode](decisions/0020-vault-path-checks.md)
  - [0021 Config format 1 and vault pinning](decisions/0021-config-format.md)
  - [0022 --init: check and ask first, create last, undo on failure](decisions/0022-init.md)
  - [0023 Unlock: one prompt, fail closed, pinned vault id](decisions/0023-unlock.md)
  - [0024 Subcommands: crystal init, add, remove](decisions/0024-subcommands.md)
  - [0025 scrypt within the locked-memory budget](decisions/0025-scrypt-budget.md)
  - [0026 Locked memory floor of 512 MiB](decisions/0026-memlock-floor-512.md)
  - [0027 scrypt: one fixed work factor, log N 18](decisions/0027-scrypt-fixed.md)
  - [0028 New passphrases: at least 20 characters](decisions/0028-passphrase-minimum.md)

# Development

- [Roadmap and status](development/roadmap.md)
- [Testing and verification](development/testing.md)
