# Roadmap

## Status (2026-11-12)

| Phase | State |
| --- | --- |
| 0 Skeleton and policy | Done |
| 1 Hardening bootstrap and terminal guard | Done |
| 2 Crypto and vault format | Done (#3 to #15); redesigned in step L1 (decision 0032): fixed vault location, no config file, `rm`, `check-code`, pins |
| 3 Shell and viewer | Done: the Crystal shell with `ls`, `tags`, `find`, `search`, `view`, `links`, `copy` and completion; renderer, sanitizer, pager, Crystal's own clipboard (decisions 0031 to 0041) |
| C Own age code | Done: Crystal encrypts and decrypts with `src/age_v1/`, its own implementation of the age v1 format (decision 0042); the `age` crate is removed, and files it wrote check the code instead (decision 0043) |
| 4 Editor | Done (E0 to E7), apart from one acceptance check: the editor (decision 0045), `edit`, `new`, `mv` with link rewriting, `rm`, saving. kill -9 during saves passes; the strace check runs in CI only and counts once CI has passed it |
| 5 to 8 | Not started; Phase 7 dropped (decision 0036) |

What runs today: `crystal init NAME` creates a vault in
`~/.local/share/crystal/NAME`, `crystal recover NAME` sets a new passphrase
with the recovery code, `crystal check-code NAME` checks the code,
`crystal rm NAME` deletes a vault, and `crystal NAME` unlocks the vault
into the Crystal shell, which knows `ls`, `tags`, `find`, `search`,
`view`, `links`, `edit`, `new`, `mv`, `rm`, `help`, `clear` and `quit`.
`edit` and `new` open the editor (Ctrl+S saves, Ctrl+E shows the note
rendered, Ctrl+Q closes).

## Steps E0 to E7 (Phase 4, the editor)

Approved 2026-11-10. Each step ends in a commit with passing checks and is
approved before it starts.

| Step | What | Why |
| --- | --- | --- |
| E0 (done) | Decision 0045 (a simple modeless editor; replaces 0041's automatic copy), this plan, the chapters | Fix the keys and the memory rules before more editor code |
| E1 (done) | `editor::buffer`: zeroizing text buffer grown by hand (copy, wipe the old one), freed bytes overwritten, capped at 8 MiB; grapheme steps; the undo log (200 steps). Pastes keep their line breaks; the shell flattens them | Every later step edits through it; property tests that undo restores the text exactly and grapheme steps land on boundaries |
| E2 (done) | Keys: Shift + arrows, Home and End; pastes up to 8 MiB with a notice when cut (the shell too); Ctrl+Z becomes a key and keyboard suspend goes (`SIGTSTP` still suspends); syscalls.md and the as-built notes | The editor's keys reach it; less special-casing in `term.rs` |
| E3 (done) | `editor::Editor`: the 0045 keys as a terminal-free state machine, like the pager | Tested key by key |
| E4 (done) | The editor's view (soft wrap, tabs, hidden characters spelled out, status line) and Ctrl+E to the viewer and back, previewing unsaved text | Drawing on the grid; property tests that no cell holds a forbidden character and the cursor stays on screen |
| E5 (done) | Saving: the session holds the notes and the recipient (the identity is wiped after unlock); `VERSION` before the first format-2 `vault.age`, generation, counter, indexes; a save over a `vault.age` changed on disk is refused; `edit NAME`, `new NAME`; the acceptance tests (strace, kill -9 during saves) | A usable editor that never writes plaintext and never leaves a broken vault |
| E6 (done) | `mv OLD NEW` rewriting `[[links]]` (keeping `\|shown text` and `#fragment`); `rm NAME`, confirmed with `y`, final | The remaining note commands of 0033 |
| E7 (done) | Phase 4 acceptance, the book finished, status updated | Ends Phase 4 |

## Steps C1 to C3 (done, before Phase 4)

Replace the `age` crate with Crystal's own implementation of the age v1
format ([0042](../decisions/0042-own-age-implementation.md)), approved
2026-11-09. Each step ends in a commit with passing checks and is approved
before it starts.

| Step | What | Why |
| --- | --- | --- |
| C1 (done) | `src/age_v1/`: base64, Bech32, header and MAC, STREAM, scrypt and X25519 stanzas, `encrypt` and `decrypt`, ported from age 0.12.1; the C2SP/CCTV vectors (`scripts/import-cctv.sh`, `tests/corpus/age-cctv/`); cross-checks with the `age` crate both ways up to 32 MiB; property tests at a million cases | Build and prove the replacement while the old code still runs |
| C2 (done) | `crypto.rs` and `vault/format.rs` switch to `src/age_v1/`; `age` becomes a dev-dependency for the cross-checks; existing vaults and identity files open unchanged; syscalls, architecture and threat-model chapters updated | Crystal encrypts with its own age code |
| C3 (done) | `age` crate removed (`secrecy` already went in C2); before that it wrote `tests/corpus/age-rage/`, which the moved tests now use; integration tests use `check-code`, `recover` and unlock; `deny.toml` has no advisory exception and one duplicate (`syn`); `Cargo.lock` 160 → 78 crates (decision 0043) | No age crate anywhere; the one check lost (age reading a 32 MiB file Crystal wrote) is listed in decision 0043 |

## Earlier steps (Phase 3)

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
| L6 (done) | L6a: indexes (names, `#tags`, `[[links]]`) and `ls`, `tags`, `find`, `search`. L6b: `view` (the pager: links with `#heading` and `#^id`, backlinks) and `links`. L6c: Crystal's own clipboard (`y`); no system clipboard ([0041](../decisions/0041-internal-clipboard-only.md)) | A usable read-only Crystal |
| L7 (done) | Tab completion; the escape-injection corpus with golden files; long property-test runs (`scripts/long-tests.sh`, weekly in CI); invisible characters refused in names ([0040](../decisions/0040-invisible-name-characters.md)) | Ends Phase 3 with its acceptance tests |

## Plan

Phases, each ending in a commit with passing acceptance tests; security layers (hardening, crypto, integrity) come before features, and the sandbox goes in once the syscall set is stable.

1. **Phase 0: Skeleton and policy.** Workspace with the `crystal` crate (modules `harden`, `term`, `cli`, `crypto`, `vault`, `backup`, `model`, `md`, `ui`, `sys`) and the `zalloc` allocator crate. Release profile, musl target, `#![forbid(unsafe_code)]`, `deny.toml`, CI running clippy with warnings denied, tests, `cargo deny check`, `cargo audit`.
   - Accept: `file crystal` reports a static binary; CI is green.
2. **Phase 1: Hardening bootstrap and terminal guard.** Steps 1 to 7 of the startup order; alt screen, raw mode, panic hook, signal handling.
   - Accept: `gdb -p` fails; `VmLck` is non-zero; a forced panic leaves a clean, cleared terminal.
3. **Phase 2: Crypto and vault format.** `crystal init` (including `identity.backup.age`, sealed to a recovery code, and `crystal recover`; decision 0029), unlock, `identity.age`, single vault blob, Padmé padding, atomic writer, generation counters in the state directory. Built with a config file and configurable vault paths; redesigned to fixed locations by decision 0032 (step L1).
   - Accept: round-trip property tests; tamper suite refuses swapped, truncated and bit-flipped vaults; an older vault needs `restore`.
4. **Phase 3: Shell and viewer.** Own screen grid (decision 0031), the Crystal shell (decision 0033), vault format 2 (decision 0034), renderer and sanitizer, the pager with Obsidian links and backlinks, full-text search, Crystal's own clipboard (decision 0041), completion. Steps above.
   - Accept: the escape-injection corpus renders as inert text; the renderer and sanitizer property tests run clean for a million cases each; no note text reaches the terminal's scrollback, stdout or disk. The shell's `lock` command moves to Phase 5, with auto-lock.
5. **Phase 4: Editor** (done; the strace check runs in CI). A simple modeless editor on the grid ([decision 0045](../decisions/0045-simple-editor.md)): arrows, Home/End, PgUp/PgDn, selection with Shift, Ctrl+C/X/V with Crystal's own clipboard (the system clipboard is the terminal's Ctrl+Shift+C/V), Ctrl+Z/Y undo and redo, Ctrl+S save, Ctrl+E to the viewer and back, Ctrl+Q close; about 850 lines. Then `edit`, `new`, `mv` (rewriting links), `rm` (confirmed, final), save (writing `VERSION` before the first format-2 `vault.age`, decision 0034), Syncthing conflicts detected, never merged: a conflict copy is refused at unlock and a save over a changed `vault.age` is refused (decision 0045). Steps E0 to E7 above.
   - Accept: `strace` shows only ciphertext writes inside the vault; kill -9 during save never corrupts the vault.
6. **Phase 5: Memory hygiene and comfort.** Zeroizing allocator, auto-lock and the shell's `lock` command, lock on signals, settings in `~/.config/crystal` (read-only plain text; decision 0032).
   - Accept: the canary scan finds no plaintext after lock.
7. **Phase 6: Sandbox.** Coverage-guided fuzzing of the renderer, sanitizer and vault decoder in a separate `fuzz/` crate (`cargo-fuzz`; the dependency approved then), run for an hour clean. Landlock and seccomp in log mode, collect syscalls, then switch to kill. SELinux policy module and optional bubblewrap launcher. Debug-only `--sandbox-selftest CHECK` and `--sandbox-status` ([Sandboxing](../design/sandboxing.md)).
   - Accept: the checks in [Sandboxing](../design/sandboxing.md) section 9 pass, including that `socket`, `execve` and opening `~/.ssh` are killed or denied.
8. ~~**Phase 7: Backups.**~~ Dropped (decision 0036): backups are left to restic, borg or any file-level tool; Crystal only accepts a restored, older vault after the word `restore` (step L3), and `crystal check-code NAME` checks the recovery code (step L1).
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), optionally a hardware key as a further recovery method (never required; decision 0029), passphrase change, a new recovery code, and key rotation (with an optional stronger scrypt setting, see Storage, Open questions), explicit plaintext export with a warning, reproducible builds, signed releases, embedded dependency list via `cargo auditable`, per-user install to `~/.local/bin` with the optional immutable flag ([Installation](../guide/installation.md)).
   - Accept: two clean builds produce identical hashes; the signature verifies.

A usable daily driver exists after Phase 4; Phases 5 and 6 are what make it meaningfully more secure than gocryptfs plus an editor.
