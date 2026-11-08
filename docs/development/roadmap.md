# Roadmap

## Status (2026-11-08)

| Phase | State |
| --- | --- |
| 0 Skeleton and policy | Done |
| 1 Hardening bootstrap and terminal guard | Done |
| 2 Crypto and vault format | Done (#3 to #15); redesigned in step L1 (decision 0032): fixed vault location, no config file, `rm`, `check-code`, pins |
| 3 Shell and viewer | In progress: own screen grid, frame writer, layout, wrapping and widgets done (decision 0031) |
| 4 to 8 | Not started; Phase 7 dropped (decision 0036) |

What runs today: `crystal init NAME` creates a vault in
`~/.local/share/crystal/NAME`, `crystal recover NAME` sets a new passphrase
with the recovery code, `crystal check-code NAME` checks the code,
`crystal rm NAME` deletes a vault, and `crystal NAME` unlocks the vault
into the Crystal shell, which so far knows `help`, `clear` and `quit`.

## Next steps (Phase 3)

Each step ends in a commit with passing checks and is approved before it
starts.

| Step | What | Why |
| --- | --- | --- |
| D1 (done) | Decisions 0032 to 0036 and the book (docs only) | Fix the design before the code changes, so every step has a written target |
| L1 (done) | Fixed locations ([0032](../decisions/0032-fixed-locations.md)), with syscalls.md and the startup chapter's as-built notes updated: `crystal init NAME`, `crystal NAME`, `crystal recover NAME`, `crystal check-code NAME`, `crystal rm NAME`; remove `add`, `remove`, `--at`, DIR, `config.age` and the configured-path walk; `NAME.pin` and the counter in the state directory | Removes about 1,200 lines and the largest untrusted-path surface before anything new is built |
| L2 (done) | Vault format 2 ([0034](../decisions/0034-vault-format-2.md)): folders, name rules, no tags field (tags live in the text), reading format 1, the `VERSION` rule | The shell addresses notes by name; the format must be settled before notes can be created |
| L3 (done) | Restore acceptance at unlock ([0036](../decisions/0036-backups-external.md)) | Makes restores from restic work; small, and it touches the unlock code L1 just simplified |
| L4 (done) | Sanitizer and `SafeText` ([0037](../decisions/0037-visible-escapes.md)); the Markdown renderer and plain text notes ([0038](../decisions/0038-plain-text-notes.md)), with bounded nesting, golden and property tests | `view` needs it; the main defence against hostile note content (A3) |
| L5 (done) | Shell core ([0033](../decisions/0033-shell.md)): prompt line in zeroizing memory, quoting, scrollback and history in memory | The frame every command runs in |
| L6 | Read-only commands: `ls`, `view` (pager: links with `#heading` and `#^id`, backlinks, `y`/`Y`), `search`, `find`, `tags`, `links`, `copy`, `help`, `clear`, `lock`, `quit`; in-memory indexes, with `#tag` and `[[link]]` parsing from the text; both clipboards ([0035](../decisions/0035-clipboard.md)) | A usable read-only Crystal with copying |
| L7 | Tab completion; end-to-end tests of every command; escape-injection corpus and renderer fuzzing | Ends Phase 3 with its acceptance tests |

## Plan

Phases, each ending in a commit with passing acceptance tests; security layers (hardening, crypto, integrity) come before features, and the sandbox goes in once the syscall set is stable.

1. **Phase 0: Skeleton and policy.** Workspace with the `crystal` crate (modules `harden`, `term`, `cli`, `crypto`, `vault`, `backup`, `model`, `md`, `ui`, `sys`) and the `zalloc` allocator crate. Release profile, musl target, `#![forbid(unsafe_code)]`, `deny.toml`, CI running clippy with warnings denied, tests, `cargo deny check`, `cargo audit`.
   - Accept: `file crystal` reports a static binary; CI is green.
2. **Phase 1: Hardening bootstrap and terminal guard.** Steps 1 to 7 of the startup order; alt screen, raw mode, panic hook, signal handling.
   - Accept: `gdb -p` fails; `VmLck` is non-zero; a forced panic leaves a clean, cleared terminal.
3. **Phase 2: Crypto and vault format.** `crystal init` (including `identity.backup.age`, sealed to a recovery code, and `crystal recover`; decision 0029), unlock, `identity.age`, single vault blob, Padmé padding, atomic writer, generation counters in the state directory. Built with a config file and configurable vault paths; redesigned to fixed locations by decision 0032 (step L1).
   - Accept: round-trip property tests; tamper suite refuses swapped, truncated and bit-flipped vaults; an older vault needs `restore`.
4. **Phase 3: Shell and viewer.** Own screen grid (decision 0031), the Crystal shell (decision 0033), vault format 2 (decision 0034), renderer and sanitizer, the pager with Obsidian links and backlinks, full-text search, both clipboards (decision 0035). Steps above.
   - Accept: the escape-injection corpus renders as inert text; renderer fuzzing runs clean for an hour; no note text reaches the terminal's scrollback, stdout or disk.
5. **Phase 4: Editor.** A small, modeless editor on the grid (nano-like: arrows, Home/End, PgUp/PgDn, selection with Shift, internal and system copy, paste, simple undo; about 650 lines), `edit`, `new`, `mv` (rewriting links), `rm` (confirmed, final), save (writing `VERSION` before the first format-2 `vault.age`, decision 0034), Syncthing conflict detection with per-note merge (newest wins per note; the other version kept as a note named "… (conflict)").
   - Accept: `strace` shows only ciphertext writes inside the vault; kill -9 during save never corrupts the vault.
6. **Phase 5: Memory hygiene and comfort.** Zeroizing allocator, auto-lock, lock on signals, settings in `~/.config/crystal` (read-only plain text; decision 0032).
   - Accept: the canary scan finds no plaintext after lock.
7. **Phase 6: Sandbox.** Landlock and seccomp in log mode, collect syscalls, then switch to kill. SELinux policy module and optional bubblewrap launcher. Debug-only `--sandbox-selftest CHECK` and `--sandbox-status` ([Sandboxing](../design/sandboxing.md)).
   - Accept: the checks in [Sandboxing](../design/sandboxing.md) section 9 pass, including that `socket`, `execve` and opening `~/.ssh` are killed or denied.
8. ~~**Phase 7: Backups.**~~ Dropped (decision 0036): backups are left to restic, borg or any file-level tool; Crystal only accepts a restored, older vault after the word `restore` (step L3), and `crystal check-code NAME` checks the recovery code (step L1).
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), optionally a hardware key as a further recovery method (never required; decision 0029), passphrase change, a new recovery code, and key rotation (with an optional stronger scrypt setting, see Storage, Open questions), explicit plaintext export with a warning, reproducible builds, signed releases, embedded dependency list via `cargo auditable`, per-user install to `~/.local/bin` with the optional immutable flag ([Installation](../guide/installation.md)).
   - Accept: two clean builds produce identical hashes; the signature verifies.

A usable daily driver exists after Phase 4; Phases 5 and 6 are what make it meaningfully more secure than gocryptfs plus an editor.
