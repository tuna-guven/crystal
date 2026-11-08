# Roadmap

## Status (2026-11-06)

| Phase | State |
| --- | --- |
| 0 Skeleton and policy | Done |
| 1 Hardening bootstrap and terminal guard | Done |
| 2 Crypto, vault format and config | Done (#3 to #15): `crystal init`, `crystal add`, `crystal remove`, unlock, `config.age`, `identity.age` and `identity.backup.age`, the vault format with Padmé padding, the atomic writer, generation counters, and the tamper suite |
| 3 to 8 | Not started |

What runs today: `crystal init NAME` creates a vault, `crystal add` and
`crystal remove` register and forget vaults, and `crystal NAME` performs startup
steps 1 to 8: it unlocks the vault and shows how many notes it holds.
Viewing and editing notes come in Phases 3 and 4.

## Plan

Nine phases, each ending in a commit with passing acceptance tests; security layers (hardening, crypto, integrity) come before features, and the sandbox goes in once the syscall set is stable.

1. **Phase 0: Skeleton and policy.** Workspace with the `crystal` crate (modules `harden`, `term`, `cli`, `crypto`, `vault`, `backup`, `model`, `md`, `ui`, `sys`) and the `zalloc` allocator crate. Release profile, musl target, `#![forbid(unsafe_code)]`, `deny.toml`, CI running clippy with warnings denied, tests, `cargo deny check`, `cargo audit`.
   - Accept: `file crystal` reports a static binary; CI is green.
2. **Phase 1: Hardening bootstrap and terminal guard.** Steps 1 to 7 of the startup order; alt screen, raw mode, panic hook, signal handling.
   - Accept: `gdb -p` fails; `VmLck` is non-zero; a forced panic leaves a clean, cleared terminal.
3. **Phase 2: Crypto, vault format and config.** `crystal init` (including `identity.backup.age`, sealed to a recovery code, and `crystal recover`; decision 0029), `crystal add`, `crystal remove`, unlock, `identity.age`, encrypted `config.age`, single vault blob, Padmé padding, atomic writer, generation counters for vaults and config in the state directory.
   - Accept: round-trip property tests; tamper suite refuses foreign, truncated, bit-flipped and rolled-back vaults and configs; a config pointing at a symlinked path, a group-writable parent, `$HOME`, `~/.ssh` (no `VERSION`), or the config or state directory is refused.
4. **Phase 3: Read-only viewer.** Fuzzy finder over titles, renderer and sanitizer, wikilink navigation, backlinks pane.
   - Accept: the escape-injection corpus renders as inert text; renderer fuzzing runs clean for an hour.
5. **Phase 4: Editor.** `tui-textarea` integration, save, new, rename, delete, Syncthing conflict detection with per-note merge.
   - Accept: `strace` shows only ciphertext writes inside the vault; kill -9 during save never corrupts the vault.
6. **Phase 5: Memory hygiene and comfort.** Zeroizing allocator, auto-lock, lock on signals, full-text search, tags.
   - Accept: the canary scan finds no plaintext after lock.
7. **Phase 6: Sandbox.** Landlock and seccomp in log mode, collect syscalls, then switch to kill. SELinux policy module and optional bubblewrap launcher. Debug-only `--sandbox-selftest CHECK` and `--sandbox-status` ([Sandboxing](../design/sandboxing.md)).
   - Accept: the checks in [Sandboxing](../design/sandboxing.md) section 9 pass, including that `socket`, `execve` and opening `~/.ssh` are killed or denied.
8. **Phase 7: Backups.** Local snapshot store, external drive marker, headless mode with udev trigger, retention, verify, restore.
   - Accept: delete the vault, restore from the drive, every note matches; a tampered snapshot is refused; a headless run copies a new vault.age without the key.
   - Accept: `crystal verify-backup NAME` asks for the recovery code and checks `identity.backup.age`: it must decrypt and hold the same identity as `identity.age`, so a wrong code on paper or a broken key backup is found before it is needed, without changing anything (Phase 2 review, S6; `crystal recover` already reads the backup, but only when it is needed).
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), optionally a hardware key as a further recovery method (never required; decision 0029), passphrase change, a new recovery code, and key rotation (with an optional stronger scrypt setting, see Storage, Open questions), explicit plaintext export with a warning, reproducible builds, signed releases, embedded dependency list via `cargo auditable`, per-user install to `~/.local/bin` with the optional immutable flag ([Installation](../guide/installation.md)).
   - Accept: two clean builds produce identical hashes; the signature verifies.

A usable daily driver exists after Phase 4; Phases 5 and 6 are what make it meaningfully more secure than gocryptfs plus an editor.
