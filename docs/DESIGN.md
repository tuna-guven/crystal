# Crystal: Encrypted Markdown TUI Design

2026-10-11, Autodidact

## Overview and goals

Crystal is a single static Rust binary that opens an age-encrypted Markdown vault in a full-screen TUI, decrypts notes only into locked memory, and never writes plaintext to disk.

**Goals**

- Obsidian-like core: a vault of Markdown notes, `[[wikilinks]]`, backlinks, tags, a fuzzy note finder and full-text search
- Two modes per note: rendered view and in-process editor
- One static musl binary, no runtime dependencies, no network code at all
- Confidentiality and integrity of notes and their titles at rest, including against whoever stores or syncs the files

**Non-goals (v1)**

- Plugins, scripting, themes beyond a fixed palette
- Sync: Syncthing or git moves ciphertext; Crystal only detects tampering and conflicts
- Images, attachments, HTML, embeds, link opening
- Mobile, multi-user vaults, collaborative editing

**Design principles**

1. Plaintext lives only in mlocked, zeroized memory inside one process.
2. Fail closed: any integrity mismatch refuses to open, never "repairs" silently.
3. Boring crypto: age only, no custom primitives, no custom KDF.
4. Deny by default: Landlock and seccomp shrink what a compromised process can do.
5. Every dependency must justify itself; the code you write is small enough to review end to end.

## Threat model

Crystal defends notes at rest and during use against storage, sync, theft and malicious content; it does not defend against root, kernel compromise or a hostile terminal emulator.

**Assets:** note content; note metadata (titles, links, tags, note count, sizes); the vault identity key; the user's passphrase or hardware key.

| Adversary | Capability | Scope | Primary defense |
| --- | --- | --- | --- |
| A1 Storage or sync attacker (cloud, Syncthing peer, stolen backup) | Read, modify, delete, reorder, roll back ciphertext files | In | age AEAD over the whole vault, embedded vault\_id, generation counter, Padmé padding |
| A2 Device thief, machine off | Full disk read | In | No plaintext ever written; full-disk encryption assumed as a second layer |
| A3 Malicious note content (imported, synced, pasted) | Terminal escape injection, parser bugs | In | Sanitizer on every text node, fuzzed parser and renderer, sandbox limits impact |
| A4 Supply chain (malicious or compromised crate) | Arbitrary code in the process | In | Minimal deps, cargo-vet, cargo-deny, sandbox blocks network and exec |
| A5 Same-user malware, vault locked | Read files, log keystrokes | Partial | Notes stay encrypted; hardware key with touch defeats passphrase logging |
| A6 Same-user malware, vault unlocked | ptrace, /proc reads, screen capture | Partial | Non-dumpable process blocks ptrace and /proc/pid/mem; screen capture not preventable |
| A7 Root, kernel exploit, cold boot, evil maid | Everything | Out | Handled at OS level (verified boot, TPM, hardened kernel) |
| A8 Shoulder surfing, screen recording | Sees the screen | Out | Idle auto-lock reduces exposure |

**Trust boundaries:** the kernel; the terminal emulator (it receives every rendered character); the age crate; the compiled binary and its build chain.

**Security properties Crystal guarantees**

- Confidentiality of note content and titles at rest
- Integrity of each note and of the vault as a set (no swapped, injected or deleted notes go unnoticed)
- Rollback detection relative to the newest state this device has seen
- No plaintext persisted by Crystal itself, including on crash

**Accepted leaks:** the vault's Padmé size bucket and when it was last saved. Note count, note sizes and which note changed stay hidden.

## Architecture

One process, one crypto boundary: everything above the age layer handles plaintext in locked memory, everything below it handles only ciphertext.

```plain
                                        +----------------------+
                                        | Terminal emulator    |
                                        +----------------------+
                                                   ^
                                                   |  keys in, frames out
                                                   v
+-- Crystal process: mlocked, non-dumpable, Landlock + seccomp ------------+
| +----------------------------------------------------------------------+ |
| | Terminal guard + UI (ratatui)                                        | |
| | alt screen, raw mode, cleared on exit and panic                      | |
| +----------------------------------------------------------------------+ |
|             |                                    ^                       |
|             v                                    v                       |
| +--------------------------------+    +--------------------------------+ |
| | Renderer + sanitizer           |    | Editor (tui-textarea)          | |
| | pulldown-cmark events          |    | edits in locked memory         | |
| | control chars stripped         |    | never spawns $EDITOR           | |
| +--------------------------------+    +--------------------------------+ |
|             ^                                    ^                       |
|             |                                    v                       |
| +----------------------------------------------------------------------+ |
| | Note model                                                           | |
| | all notes, titles, links and tags, in zeroizing memory               | |
| +----------------------------------------------------------------------+ |
|                                    ^                                     |
|                                    |  plaintext                          |
|                                    v                                     |
| +----------------------------------------------------------------------+ |
| | Crypto (age crate, rage)   <== CRYPTO BOUNDARY                       | |
| | identity in SecretBox; decrypts on unlock, encrypts on save          | |
| | unlocked by passphrase (scrypt) or YubiKey plugin, before sandbox    | |
| +----------------------------------------------------------------------+ |
|                                    ^                                     |
|                                    |  ciphertext                         |
|                                    v                                     |
| +----------------------------------------------------------------------+ |
| | Vault I/O                                                            | |
| | single vault blob, Padme padding, atomic rename writes               | |
| +----------------------------------------------------------------------+ |
+--------------------------------------------------------------------------+
                                     ^
                                     |  ciphertext only
                                     v
                  +------------------------------------+
                  | Vault directory                    |
                  | vault.age and identity.age only    |
                  +------------------------------------+
                                     |  copied, never decrypted
                                     v
                  +------------------------------------+
                  | Backup snapshots                   |
                  | external drive, ciphertext copies  |
                  +------------------------------------+
```

The unlock step runs before the sandbox because age plugins are separate binaries; once the vault identity is in memory, exec is denied for the rest of the session.

**On-disk layout**

```text
vault/
  VERSION        plaintext: format version only
  identity.age   vault X25519 identity, encrypted to your passphrase and/or hardware key(s)
  vault.age      the entire vault: every note, title, link and tag, padded and encrypted
~/.local/state/crystal/<vault_id>.gen   last seen vault generation (local only, never synced)
```

**Key design.** `crystal --init` generates a random vault X25519 identity and encrypts it to one or more unlock recipients: a scrypt passphrase, a YubiKey or TPM plugin recipient, and an offline backup recipient. `vault.age` is encrypted to the vault recipient. Changing the passphrase or adding a key rewrites only `identity.age`.

**Vault contents (postcard, then Padmé-padded and encrypted):** format version, `vault_id` (32 random bytes), `generation` (u64, +1 per save), and every note with its id, title, tags, created, modified and Markdown body.

**Integrity.** One age stream authenticates every note at once, so swapping, injecting or deleting single notes is impossible by construction. The embedded `vault_id` rejects a vault from elsewhere, and the local generation file rejects an older copy (rollback).

**Save sequence:** serialize, pad, encrypt to a temp file, `fsync`, rename over `vault.age`, `fsync` the directory. The rename is the commit point; a crash leaves either the old vault or the new one, never a mix.

**Costs of one blob, accepted:** every save re-encrypts the whole vault (milliseconds for a few MB of text); all notes sit decrypted in locked memory while unlocked, so auto-lock matters more; a Syncthing conflict produces a second `vault.sync-conflict-*.age`, which Crystal decrypts at unlock and merges per note, flagging notes edited on both sides.

**Open questions**

- Post-quantum: check whether the `age` crate version you pin supports a hybrid ML-KEM recipient type. If it does, use it for the vault recipient, since synced ciphertext is exposed to harvest-now, decrypt-later. Go age 1.3+ has native ML-KEM-768 + X25519 recipients; until rage ships the same type natively, avoid unaudited third-party PQ crates, and avoid PQ via a plugin, since notes are encrypted on every save and plugins need execve.
- Per-note files versus one encrypted vault blob: decided, one vault blob. It hides note count, sizes and which note changed, at the cost of whole-vault conflicts and all notes in memory while unlocked.

## Crate selection

About a dozen direct dependencies, all pure Rust, no async runtime, no C libraries. Pin exact versions in `Cargo.lock` and check each crate's compatibility with the ratatui version you choose.

| Need | Crate | Notes |
| --- | --- | --- |
| TUI layout and widgets | `ratatui` | Immediate-mode rendering; use only the widgets you need |
| Terminal backend | `crossterm` | Raw mode, alternate screen, events, bracketed paste |
| Editor widget | `tui-textarea` | Start here; replace with your own on `ropey` if its copying worries you |
| Markdown parser | `pulldown-cmark` | `default-features = false`, no HTML feature; you write the renderer |
| Encryption | `age` | The library crate of the rage project; X25519 and scrypt recipients; enable the plugin feature only for YubiKey or TPM unlock |
| Secret types | `secrecy`, `zeroize` | `SecretBox` for keys, `Zeroizing<Vec<u8>>` and derive `ZeroizeOnDrop` for plaintext |
| Hashing | `sha2` | Already in age's tree; used for backup snapshot hashes |
| Serialization | `serde` + `postcard` | Compact, schema-light, small attack surface for the vault contents |
| Syscalls | `rustix` | prctl, setrlimit, mlockall, fsync, renameat without writing `unsafe` yourself |
| Filesystem sandbox | `landlock` | Restrict file access to the vault and state directories |
| Syscall filter | `seccompiler` | Pure Rust BPF builder from Firecracker; avoids the C libseccomp |
| CLI args | `lexopt` | Tiny; you only parse a vault path and a few flags |

**About rage:** rage is the Rust implementation of age, and its library ships as the `age` crate in the rage repository. Link that crate directly; never shell out to the `rage` binary, which would need `execve` and push plaintext through pipes.

**Avoid:** `tokio` or any async runtime, HTTP clients, `clap` (large for this job), syntax highlighters (`syntect`), `tui-markdown` or other ready-made renderers, image crates, file watchers, logging crates that write files, `rpassword` (read the passphrase yourself in raw mode).

**Release profile:** `lto = true`, `codegen-units = 1`, `strip = true`, `overflow-checks = true`, keep `panic = "unwind"` so `Drop` zeroizes on panic. Target `x86_64-unknown-linux-musl` for a static binary. Put `#![forbid(unsafe_code)]` on the crate. The zeroizing allocator, the only `unsafe` code, lives in a separate workspace crate `crates/zalloc`; the `sys` module only installs it.

## Build vs reuse

Reuse anything that is hard to get right and well reviewed (crypto, parsing, terminal I/O); build everything that defines Crystal's security properties.

| Component | Decision | Size estimate | Why |
| --- | --- | --- | --- |
| Encryption primitives | Reuse `age` | n/a | Never roll your own crypto |
| Markdown parsing | Reuse `pulldown-cmark` | n/a | Parsers are where bugs live |
| Terminal I/O and layout | Reuse `crossterm` + `ratatui` | n/a | Mature, widely used |
| Sandbox plumbing | Reuse `landlock` + `seccompiler` | n/a | Kernel ABI details are easy to get wrong |
| Vault format | Build | 600 lines | Defines integrity, rollback and metadata hiding |
| Atomic writer | Build | 80 lines | Ciphertext-only, temp + fsync + rename + dir fsync |
| Markdown renderer | Build | 300 lines | Small subset, full control over output |
| Sanitizer | Build | 60 lines | Strips C0, C1, ESC, BiDi overrides from every rendered span |
| Wikilink and backlink index | Build | 200 lines | Simple map over titles, rebuilt on unlock |
| Fuzzy finder and search | Build | 150 lines | Subsequence scorer plus case-insensitive substring search |
| Terminal guard | Build | 120 lines | Alt screen, raw mode, clear on exit, panic hook, signals |
| Hardening bootstrap | Build | 150 lines | prctl, rlimits, mlockall, env scrub, tty checks |
| Passphrase prompt | Build | 60 lines | Reads from /dev/tty in raw mode into zeroizing memory |
| Zeroizing allocator | Build | 40 lines | GlobalAlloc wrapper that zeroes on free and on realloc |
| Editor | Reuse first, maybe build later | 800+ lines if built | `tui-textarea` is fine for v1 |

The zeroizing allocator is the key trick for Rust. `String` and `Vec` growth reallocates and leaves old copies in freed heap where `zeroize` cannot reach them. A global allocator that zeroes every block before freeing it closes that gap for the whole program, including inside `tui-textarea` and `pulldown-cmark`. The cost is negligible for a notes app.

## Plaintext leakage prevention

Every path plaintext could take out of the process is listed below with its mitigation and a concrete way to verify it.

| Leak path | Mitigation | How to verify |
| --- | --- | --- |
| Editor temp, swap, undo files | In-process editor only; never spawn `$EDITOR` | `strace -f -e trace=openat,write` shows writes only to `*.age` and temp ciphertext |
| Stale heap copies after realloc | Zeroizing global allocator; `Zeroizing` buffers for all plaintext | Canary scan test (Testing section) |
| Swap and hibernation | `mlockall` with current, future and on-fault flags; raise or check `RLIMIT_MEMLOCK`; refuse to unlock if it fails | `grep VmLck /proc/<pid>/status` is non-zero |
| Core dumps | `RLIMIT_CORE = 0` and `PR_SET_DUMPABLE = 0` as the first lines of `main` | `kill -SEGV` during a test build produces no core |
| Same-user ptrace and /proc/pid/mem | `PR_SET_DUMPABLE = 0`; system Yama `ptrace_scope` at 1 or higher | `gdb -p <pid>` fails as your user |
| Terminal scrollback | Alternate screen only; clear it before leaving; never print note text on the main screen | Scroll up after quitting: nothing |
| tmux or screen history | Warn when `TMUX` or `STY` is set | Manual check |
| Terminal title | Never emit OSC 0/2 with a note title | Code review rule |
| Clipboard | No clipboard support in v1; later opt-in OSC 52 with auto-clear | n/a |
| Shell history, argv, environment | Passphrase only from /dev/tty; no secrets in args or env; ignore `CRYSTAL_*` secret variables | `cat /proc/<pid>/cmdline` |
| Error messages and panics | Errors never include note text; panic hook prints location only, not the payload | Grep for `format!` near content types; fuzz panics |
| Crash mid-write | Plaintext is never written, so a crash can only leave ciphertext temp files | Kill -9 during save, inspect directory |
| Filenames and titles | One vault.age file; no per-note names or sizes exist on disk | `ls vault/` |
| Note sizes | Padmé padding before encryption | Vault size changes only between Padmé buckets |
| Ctrl+Z and terminal close | Lock on `SIGTSTP`, `SIGHUP`, `SIGTERM`: zeroize model, leave alt screen, then stop or exit | Suspend and inspect screen |
| Idle sessions | Auto-lock after N minutes of no input | Timer test |
| Persistent scrollback in the emulator | Alt screen content is not kept in scrollback by most emulators; prefer a terminal without disk-backed history | Check your emulator's docs |

Type-level rule that enforces most of this: note content only ever exists as a `NoteText` newtype over `Zeroizing<String>` that implements neither `Debug`, `Display` nor `Serialize`. The only way out is the renderer, the editor, or the encryptor.

## Process hardening and sandboxing

Hardening happens in a fixed order: protections that cost nothing go first, unlock happens while exec is still possible (for hardware-key plugins), and the sandbox locks down before any note is decrypted or rendered.

1. Install the panic hook (restore terminal, print location only).
2. `setrlimit(RLIMIT_CORE, 0)`, `prctl(PR_SET_DUMPABLE, 0)`, `prctl(PR_SET_NO_NEW_PRIVS, 1)`, `umask(0o077)`.
3. Environment allowlist: read only `TERM`, `COLORTERM` and locale variables, through one function; never read `EDITOR`, `PAGER` or similar. The process environment is not modified (`remove_var` is `unsafe` in edition 2024 and the crate forbids `unsafe`); instead, the only child process, the age plugin, is spawned with `env_clear()` plus the allowlist.
4. `mlockall`; refuse to continue if it fails, with a message about `RLIMIT_MEMLOCK`.
5. Require a real tty; open `/dev/tty` directly for input and output.
6. Parse args (vault path, `--init`, `--lock-after`).
7. Open the vault and state directories as directory fds with `O_DIRECTORY` and `O_NOFOLLOW`.
8. Unlock: decrypt `identity.age` with a passphrase or an age plugin. Plugins are external binaries, so this is the last moment `execve` is allowed.
9. Apply Landlock: read and write only beneath the vault, state and backup directories; on ABI 4 or newer, also deny TCP bind and connect; on ABI 6 or newer, scope signals and abstract Unix sockets. Fail closed if Landlock is unsupported, unless the user passes an explicit `--no-landlock` flag.
10. Apply seccomp (allowlist, below), with kill-process as the default action.
11. Decrypt the vault, build the index, enter the TUI.

**Seccomp allowlist (starting point):** `read`, `write`, `openat`, `close`, `fstat`, `newfstatat`, `statx`, `lseek`, `getdents64`, `mkdirat`, `renameat2`, `unlinkat`, `fsync`, `fdatasync`, `mmap`, `munmap`, `mremap`, `mprotect` (deny `PROT_EXEC`), `brk`, `madvise`, `rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `sigaltstack`, `ioctl` (only `TCGETS`, `TCSETS*`, `TIOCGWINSZ`), `poll`, `ppoll`, `epoll_*`, `clock_gettime`, `clock_nanosleep`, `futex`, `getrandom`, `kill` and `tgkill` (own pid only, for `SIGTSTP`), `exit`, `exit_group`.

**Explicitly denied:** `socket`, `connect`, `execve`, `execveat`, `ptrace`, `process_vm_readv`, `process_vm_writev`, `memfd_create`, `io_uring_*`, `bpf`, `userfaultfd`, `personality`, and `clone`/`clone3` if you stay single-threaded.

Develop with the default action set to log, run every feature under `strace -f -c` to collect the real syscall set, then switch to kill. Check whether `crossterm`'s signal handling starts a thread; if it does, allow `clone3` with thread flags only.

**System-level recommendations:** encrypted swap or zram only, hibernation disabled, Yama `ptrace_scope` at 1 or higher, full-disk encryption, and a terminal emulator without disk-backed scrollback.

## Backups

Crystal backs up automatically and fully offline by copying ciphertext only, so backups never need the key and never hold plaintext.

| Layer | Location | When it runs | Protects against |
| --- | --- | --- | --- |
| Local snapshots | `vault/.snapshots/` on the same disk | After every save | Accidental edits and deletes, bad sync merges |
| External drive | A drive you plug in, marked by an encrypted `crystal-backup.age` file bound to the `vault_id` | On unlock, on lock, and by a udev or systemd user trigger when the drive is mounted | Disk failure, theft, a wiped laptop |
| Key backup | `identity.age` also encrypted to an offline backup recipient (paper key or a second YubiKey) | At init and after every key rotation | Losing the passphrase or hardware key |

**Store format.** Each backup location holds `snapshots/<UTC timestamp>-<sha256 prefix>.age`, byte-for-byte copies of `vault.age`, plus a `SHA256SUMS` file. A text vault of a thousand notes is a few MB, so full copies are cheap; add delta storage only if the vault grows large.

**Headless mode.** `crystal backup --headless` copies `vault.age` when its hash is new and verifies the copy. It never unlocks or encrypts anything, so it needs no secrets and is safe to run from a udev rule when the drive appears; backups happen even when Crystal is closed. It runs under the same Landlock and seccomp policy, scoped to the vault and the backup drive.

**Append-only.** Crystal only adds snapshots. The single exception is retention pruning, which runs after a successful verify and never touches the newest 10 snapshots. Default retention: every snapshot for 7 days, one per day for 30 days, one per month for 12 months.

**Verification.** On each interactive unlock, Crystal decrypts the newest snapshot on every reachable backup and compares it with the live vault. `crystal backup verify --full` decrypts everything. A failed verify shows a warning on the status line and blocks pruning.

**Restore.** `crystal restore --from <path> --snapshot <timestamp>` writes the restored state as a new generation above the current one, so rollback detection stays valid. Restores are always explicit, never automatic, and refuse snapshots whose `vault_id` does not match.

**Operational rules.** Unplug the drive between runs, since a mounted drive is reachable by anything running as your user. Put LUKS on the drive to also hide note count and snapshot history. Keep a second drive off-site if you can. Test a full restore to a scratch directory once a month.

## Phased build plan

Nine phases, each ending in a commit with passing acceptance tests; security layers (hardening, crypto, integrity) come before features, and the sandbox goes in once the syscall set is stable.

1. **Phase 0: Skeleton and policy.** Single crate with modules `harden`, `term`, `crypto`, `vault`, `backup`, `model`, `md`, `ui`, `sys`. Release profile, musl target, `#![forbid(unsafe_code)]`, `deny.toml`, CI running clippy with warnings denied, tests, `cargo deny check`, `cargo audit`.
   - Accept: `file crystal` reports a static binary; CI is green.
2. **Phase 1: Hardening bootstrap and terminal guard.** Steps 1 to 7 of the startup order; alt screen, raw mode, panic hook, signal handling.
   - Accept: `gdb -p` fails; `VmLck` is non-zero; a forced panic leaves a clean, cleared terminal.
3. **Phase 2: Crypto and vault format.** `crystal --init` (including the offline backup recipient), unlock, `identity.age`, single vault blob, Padmé padding, atomic writer, generation counter in the state directory.
   - Accept: round-trip property tests; tamper suite refuses foreign, truncated, bit-flipped and rolled-back vaults.
4. **Phase 3: Read-only viewer.** Fuzzy finder over titles, renderer and sanitizer, wikilink navigation, backlinks pane.
   - Accept: the escape-injection corpus renders as inert text; renderer fuzzing runs clean for an hour.
5. **Phase 4: Editor.** `tui-textarea` integration, save, new, rename, delete, Syncthing conflict detection with per-note merge.
   - Accept: `strace` shows only ciphertext writes inside the vault; kill -9 during save never corrupts the vault.
6. **Phase 5: Memory hygiene and comfort.** Zeroizing allocator, auto-lock, lock on signals, full-text search, tags.
   - Accept: the canary scan finds no plaintext after lock.
7. **Phase 6: Sandbox.** Landlock and seccomp in log mode, collect syscalls, then switch to kill.
   - Accept: integration tests that try `socket`, `execve` and opening `~/.ssh` are killed or denied.
8. **Phase 7: Backups.** Local snapshot store, external drive marker, headless mode with udev trigger, retention, verify, restore.
   - Accept: delete the vault, restore from the drive, every note matches; a tampered snapshot is refused; a headless run copies a new vault.age without the key.
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), passphrase change and key rotation, explicit plaintext export with a warning, reproducible builds, signed releases, embedded dependency list via `cargo auditable`.
   - Accept: two clean builds produce identical hashes; the signature verifies.

A usable daily driver exists after Phase 4; Phases 5 and 6 are what make it meaningfully more secure than gocryptfs plus an editor.

## Testing and verification

Each security property gets a test that would fail if the property broke; tests run in CI on every commit.

| Test | What it proves | Tooling |
| --- | --- | --- |
| Round-trip properties | Encrypt then decrypt returns the same note; padding is reversible | `proptest` |
| Tamper suite | A vault with another vault\_id, truncation, bit flips, an older generation: all refused | Integration tests over a temp vault |
| Parser and renderer fuzzing | No panics or hangs on hostile Markdown; sanitizer output contains no control bytes | `cargo-fuzz` targets for vault decode and render |
| Escape-injection corpus | OSC 52 clipboard writes, OSC 8 links, title changes, cursor moves, BiDi tricks are all inert | Golden-file tests on rendered output |
| Disk leak test | Only `*.age` and ciphertext temp files are ever created | `strace -f`, `inotifywait -r` on home and /tmp during a scripted session |
| Canary scan | After lock, a unique canary string from a note no longer exists in process memory | Test-only feature that reads `/proc/self/mem` before dumpable is cleared |
| Hardening checks | Non-dumpable, no core, memory locked, no new privs | Integration test reading `/proc/self/status` |
| Sandbox checks | Denied syscalls kill the process; paths outside the vault return EACCES | Child-process tests |
| Unsafe code | The allocator is sound | `cargo miri test` on `zalloc` |
| Dependency audit | No known vulnerabilities, licenses allowed, crates reviewed | `cargo audit`, `cargo deny`, `cargo vet`, `cargo geiger` |
| Reproducibility | Same source gives the same binary | Two clean builds, compare SHA-256 |

Because this is a security tool, plan for an outside review once Phase 6 is done: a classmate, a security-focused community review, or a small paid audit before you rely on it for anything sensitive.

## Working with Claude Code

Put the rules below in `CLAUDE.md` at the repo root, export this doc as Markdown into `docs/DESIGN.md`, and run one phase per session in plan mode.

```markdown
# Crystal rules

Read docs/DESIGN.md before any change. Work on one phase at a time.

## Hard rules
- Never add a dependency without asking first; state the crate, version and why.
- No network code, no async runtime, no threads unless approved.
- No `unsafe` outside crates/zalloc. The crystal crate is #![forbid(unsafe_code)].
- Note content only lives in `NoteText` (Zeroizing<String>). Never derive or
  implement Debug, Display or Serialize for it. Never put note text in
  errors, logs, panics or format! strings.
- Never write files except through vault::atomic_write or backup::store, and only ciphertext.
- Never spawn processes, except the age plugin during unlock, before the sandbox.
- Any new syscall needs a line in docs/SYSCALLS.md explaining why.
- Never weaken Landlock, seccomp or hardening to make a test pass.

## Before finishing a task
- cargo clippy --all-targets -- -D warnings
- cargo test
- cargo deny check
- List what changed and which acceptance criteria now pass.
```

**Prompts per phase** (one session each):

1. Phase 0: "Scaffold the crate per DESIGN.md Phase 0. Set up the release profile, musl target, deny.toml and CI. No features yet."
2. Phase 1: "Implement the startup order steps 1 to 7 and the terminal guard. Add tests for every Phase 1 acceptance criterion."
3. Phase 2: "Implement the vault format exactly as specified in the Architecture section. Write the tamper suite first, then the code."
4. Phase 3 onward: "Implement Phase N per DESIGN.md. Start by listing the acceptance tests you will write."

After each phase, ask a fresh session to review the diff as an attacker: "Review this diff against the threat model. Find any path where plaintext can reach disk, the terminal outside the alt screen, an error message, or another process."
