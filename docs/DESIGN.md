# Crystal: Encrypted Markdown TUI Design

2026-10-11, Autodidact

## Overview and goals

Crystal is a single static Rust binary that opens an age-encrypted Markdown vault in a full-screen TUI, decrypts notes only into locked memory, and never writes plaintext to disk.

**Goals**

- Obsidian-like core: a vault of Markdown notes, `[[wikilinks]]`, backlinks, tags, a fuzzy note finder and full-text search
- Two modes per note: rendered view and in-process editor
- One static musl binary, installed per user in `~/.local/bin` without root, no runtime dependencies, no network code at all
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
4. Deny by default: Landlock and seccomp shrink what a compromised process can do; an SELinux policy module and an optional bubblewrap launcher add outer layers (docs/SANDBOXING.md).
5. Every dependency must justify itself; the code you write is small enough to review end to end.

## Threat model

Crystal defends notes at rest and during use against storage, sync, theft and malicious content; it does not defend against root, kernel compromise or a hostile terminal emulator.

**Assets:** note content; note metadata (titles, links, tags, note count, sizes); the config file (vault names, locations, pinned ids); the vault identity key; the user's passphrase or hardware key.

| Adversary | Capability | Scope | Primary defense |
| --- | --- | --- | --- |
| A1 Storage or sync attacker (cloud, Syncthing peer, stolen backup) | Read, modify, delete, reorder, roll back ciphertext files | In | age AEAD over the whole vault, embedded vault\_id, generation counter, Padmé padding |
| A2 Device thief, machine off | Full disk read | In | No plaintext ever written; full-disk encryption assumed as a second layer |
| A3 Malicious note content (imported, synced, pasted) | Terminal escape injection, parser bugs | In | Sanitizer on every text node, fuzzed parser and renderer, sandbox limits impact |
| A4 Supply chain (malicious or compromised crate) | Arbitrary code in the process | In | Minimal deps, cargo-vet, cargo-deny, sandbox blocks network and exec |
| A5 Same-user malware, vault locked | Read files, log keystrokes, replace or shadow `~/.local/bin/crystal`, write `config.age` | Partial | Notes stay encrypted; hardware key with touch defeats passphrase logging; optional immutable binary (Installation); config paths checked as untrusted (Config file) |
| A6 Same-user malware, vault unlocked | ptrace, /proc reads, screen capture | Partial | Non-dumpable process blocks ptrace and /proc/pid/mem; screen capture not preventable |
| A7 Root, kernel exploit, cold boot, evil maid | Everything | Out | Handled at OS level (verified boot, TPM, hardened kernel) |
| A8 Shoulder surfing, screen recording | Sees the screen | Out | Idle auto-lock reduces exposure |

**Trust boundaries:** the kernel; the terminal emulator (it receives every rendered character); the age crate; the compiled binary and its build chain; the installed copy in `~/.local/bin`, which is user-writable unless marked immutable (Installation).

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
| | alt screen, raw mode, cleared on exit, panic and signals             | |
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
                  | vault.age and identity*.age only   |
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

Vaults are opened by name. An encrypted per-device config file, `~/.config/crystal/config.age`, maps each name to a vault directory and holds other metadata. `crystal --init NAME` creates the vault in `~/Vaults/<name>` by default. With `--at DIR` it can go elsewhere, for example a synced folder. Every path in the config is checked at startup as if it were attacker-written (see **Config file**), so the config can point Crystal at a vault but cannot widen the sandbox beyond one checked, private directory.

```text
~/.config/crystal/            mode 0700, local only, never synced
  config.age     vault names, paths, pinned vault_ids, settings; padded and encrypted
~/Vaults/<name>/              default vault location, mode 0700 (any checked path via --at)
  VERSION        plaintext: format version only
  identity.age   vault X25519 identity, encrypted to your passphrase or to your hardware key(s)
  identity.backup.age  the same identity, encrypted to the offline backup recipient(s)
  vault.age      the entire vault: every note, title, link and tag, padded and encrypted
~/.local/state/crystal/<vault_id>.gen   last seen vault generation (local only, never synced)
~/.local/state/crystal/config.gen       last seen config generation
```

**Config file.** The config is local to one device, because paths differ between machines; a vault directory stays self-contained and syncable without it. If the config is lost, nothing is lost: `crystal --add NAME DIR` registers an existing vault again.

- *Contents (postcard, then Padmé-padded, then age):* format version, `generation` (u64, +1 per write), default `lock_after`, and per vault: name, absolute path, pinned `vault_id` (filled in at first unlock), optional `lock_after`; from Phase 7, the backup drive markers. It holds nothing that loosens security: no sandbox switches, no executable paths, no editor, no environment.
- *Encryption:* encrypted with the `age` crate, never by shelling out to `rage`, to one unlock method only. The choices are a scrypt passphrase or a hardware-key plugin recipient. age allows a scrypt stanza only as the sole recipient, and the config needs no backup recipient because it can be rebuilt. With a passphrase, Crystal prompts once. It decrypts `config.age`, then the vault's `identity.age`, with the same passphrase held in a `Zeroizing` buffer, then wipes the buffer. With a plugin, each file costs one touch.
- *Integrity limits:* age authenticates the content but not who wrote it. Under a scrypt passphrase, only the passphrase holder can produce a config that decrypts. Under a public-key or plugin recipient, anyone who knows the recipient can write a valid `config.age`. So Crystal never trusts the config for a security decision. Each vault path must be absolute, with no `..`. It is opened one component at a time from `/` with `O_NOFOLLOW`. Every directory on the way must be owned by root or by you and must not be group- or world-writable. The vault directory itself must be yours, mode 0700, and contain `VERSION`. It must not be `$HOME`, an ancestor of the config or state directory, or inside either. The pinned `vault_id` detects a swapped vault directory, but only as strongly as the config is authenticated.
- *Rollback:* `config.gen` in the state directory rejects an older config, the same way as for vaults.
- *Writes:* only `--init`, `--add` and `--remove` rewrite the config, through `vault::atomic_write`, and then exit. A normal session reads the config before the sandbox and has no write access to `~/.config/crystal` afterwards.
- *SELinux:* the policy labels `~/Vaults` statically. A vault at any other path needs a one-time `semanage fcontext` rule (docs/SANDBOXING.md). Without the rule, that vault runs without the SELinux layer, and `--sandbox-status` says so.

**Key design.** `crystal --init` generates a random vault X25519 identity and stores it twice. `identity.age` holds it for daily unlock, encrypted either to a scrypt passphrase alone or to one or more YubiKey or TPM plugin recipients. `identity.backup.age` holds it for recovery, encrypted to the offline backup recipient(s). Two files are needed because age accepts a scrypt stanza only as the sole recipient of a file, so a passphrase and a backup key cannot share one. `vault.age` is encrypted to the vault recipient. Changing the passphrase or adding a hardware key rewrites only `identity.age`; rotating the backup key rewrites only `identity.backup.age`.

**Vault contents (postcard, then Padmé-padded and encrypted):** format version, `vault_id` (32 random bytes), `generation` (u64, +1 per save), and every note with its id, title, tags, created, modified and Markdown body.

**Integrity.** One age stream authenticates every note at once, so swapping, injecting or deleting single notes is impossible by construction. The embedded `vault_id` rejects a vault from elsewhere, and the local generation file rejects an older copy (rollback).

**Save sequence:** serialize, pad, encrypt to a temp file, `fsync`, rename over `vault.age`, `fsync` the directory. The rename is the commit point; a crash leaves either the old vault or the new one, never a mix.

**Costs of one blob, accepted:** every save re-encrypts the whole vault (milliseconds for a few MB of text); all notes sit decrypted in locked memory while unlocked, so auto-lock matters more; a Syncthing conflict produces a second `vault.sync-conflict-*.age`, which Crystal decrypts at unlock and merges per note, flagging notes edited on both sides.

**Open questions**

- Post-quantum: check whether the `age` crate version you pin supports a hybrid ML-KEM recipient type. If it does, use it for the vault recipient, since synced ciphertext is exposed to harvest-now, decrypt-later. Go age 1.3+ has native ML-KEM-768 + X25519 recipients; until rage ships the same type natively, avoid unaudited third-party PQ crates, and avoid PQ via a plugin, since notes are encrypted on every save and plugins need execve.
  - *Answer (2026-11-06, #4):* not yet; the vault recipient stays X25519. The pinned `age` 0.11.5 has only X25519, scrypt and plugin recipients. The newer 0.12 line adds `tagpq` (`mlkem768p256tag`), but it is encryption-only: decrypting needs a hardware tag through a plugin, so it cannot be the vault's own recipient. 0.12 also pulls in `p256` and `crypto-bigint` 0.5, which has a timing advisory (RUSTSEC-2026-0387), for that feature alone. Neither line has a native hybrid ML-KEM-768 + X25519 identity like Go age 1.3's. Revisit when rage ships one: upgrade `age` and re-encrypt the vault to the new recipient on the next save.
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
| Filesystem sandbox | `landlock` | Restrict file access to the vault and state directories. Version 0.4.7 supports Landlock ABI up to 9; no raw Landlock syscalls, which would need `unsafe` (docs/SANDBOXING.md 5.1) |
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
| Terminal scrollback | Alternate screen only; clear it before leaving; never print note text on the main screen. All UI output goes to `/dev/tty`: clippy.toml bans `print!`, `println!`, `eprint!`, `eprintln!`, `dbg!` and `std::io::{stdin, stdout, stderr}` except in `main`'s error line and the panic hook, so a redirected stdout can never receive a frame | Scroll up after quitting: nothing; clippy in CI |
| tmux or screen history | Warn when `TMUX` or `STY` is set | Manual check |
| Terminal title | Never emit OSC 0/2 with a note title | Code review rule |
| Clipboard | No clipboard support in v1; later opt-in OSC 52 with auto-clear | n/a |
| Shell history, argv, environment | Passphrase only from /dev/tty; no secrets in args or env; ignore `CRYSTAL_*` secret variables | `cat /proc/<pid>/cmdline` |
| Error messages and panics | Errors never include note text or argv text; panic hook prints location only, not the payload; types holding argv text (`VaultName`) have a Debug that prints `VaultName(..)` | Grep for `format!` near content types; fuzz panics; `tests/startup.rs` checks that panic, panic-in-Drop and stack overflow never print the message |
| Crash mid-write | Plaintext is never written, so a crash can only leave ciphertext temp files | Kill -9 during save, inspect directory |
| Filenames and titles | One vault.age file; no per-note names or sizes exist on disk | `ls vault/` |
| Note sizes | Padmé padding before encryption | Vault size changes only between Padmé buckets |
| Ctrl+Z, terminal close, kill signals | Lock on `SIGTSTP`; every catchable signal whose default kills the process (`term::EXIT_SIGNALS` and real-time signals) and a hung-up terminal end Crystal through the normal exit path: zeroize model, leave alt screen, then stop or exit. Exits that cannot be caught are listed under "Exits that skip cleanup" | `exit_signals_restore_terminal`, `terminal_hangup_exits_cleanly`, `suspend_restores_terminal_and_resume_reenters` |
| Idle sessions | Auto-lock after N minutes of no input | Timer test |
| Persistent scrollback in the emulator | Alt screen content is not kept in scrollback by most emulators; prefer a terminal without disk-backed history | Check your emulator's docs |

Type-level rule that enforces most of this: note content only ever exists as a `NoteText` newtype over `Zeroizing<String>` that implements neither `Debug`, `Display` nor `Serialize`. The only way out is the renderer, the editor, or the encryptor.

### Exits that skip cleanup

The terminal guard and every `Drop` impl (zeroizing, from Phase 5) run only when Crystal leaves through its normal exit path or through a panic that unwinds. Phase 1 routes everything catchable onto that path; what remains cannot be caught, or cannot be caught without `unsafe` in a process that is already corrupted.

| Way out | What runs | Terminal afterwards | Notes in memory |
| --- | --- | --- | --- |
| Normal quit; terminal hangup (window closed); catchable kill signals: `SIGTERM`, `SIGHUP`, `SIGINT`, `SIGQUIT`, `SIGUSR1`, `SIGUSR2`, `SIGALRM`, `SIGVTALRM`, `SIGPROF`, `SIGXCPU`, `SIGXFSZ`, `SIGIO`, `SIGPWR`, `SIGSTKFLT`, real-time 35 to 64 | Handler writes one byte to a self-pipe; the main loop returns; every `Drop` | Cleared and restored | Zeroized |
| Panic that unwinds | Panic hook (restore, location only), then every `Drop` | Cleared and restored | Zeroized |
| Panic inside `Drop` during unwinding | Panic hook, then abort | Cleared and restored by the hook | Not zeroized |
| `SIGKILL`; real-time 32 to 34; stack overflow; other faults (`SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGTRAP`); `abort()`, including a failed allocation; seccomp kill (Phase 6) | Nothing | Raw mode, alternate screen, last frame still visible | Not zeroized |
| `SIGSTOP`, `SIGTTIN`, `SIGTTOU` (stop, not exit) | Nothing until `SIGCONT` | Last frame visible while stopped | Kept, still locked |

**Why each is accepted**

- `SIGKILL` and `SIGSTOP` cannot be caught by any process.
- Real-time signals 32 to 34: musl reserves them (thread cancellation, timers, `setxid` synchronisation) and its `sigaction` refuses handlers with `EINVAL`. `term::Signals::register` accepts that refusal for these three numbers only; any other failure stops startup.
- Faults: returning from a handler reruns the faulting instruction, and restoring the terminal from inside a `SIGSEGV` handler needs an alternate signal stack and async-signal-safe raw code, which means `unsafe` running in a process whose state is already broken. signal-hook refuses to register them. On musl, Rust's stack guard prints nothing; a stack overflow is a bare `SIGSEGV` (exit 139).
- `abort()` re-raises `SIGABRT` with the default action after any handler returns; nothing can stop it.
- `SIGTTIN` and `SIGTTOU`: the kernel raises them again each time it restarts the terminal call that caused them, so a handler would loop forever. Their default only stops the process.
- seccomp kill: killing at once, with no code running afterwards, is the point (docs/SANDBOXING.md).

**What still holds on every one of these paths**

- Nothing is printed: no exit path prints a panic message (`tests/startup.rs`, `abort_paths_print_no_secret_and_leave_no_core`).
- No core dump: `RLIMIT_CORE` is 0 (hard limit too) and the process is non-dumpable from the first lines of `main`.
- No swap: memory is `mlockall`ed. The kernel frees the pages and zeroes them before any other process gets them (on secureblue, `init_on_free=1` zeroes them at once).
- Nothing on disk: Crystal never writes plaintext, so a kill can only leave ciphertext (`kill_9_leaves_no_new_files`; from Phase 4, also during a save).

**Residual risk:** the last frame stays on the alternate screen and the terminal stays raw until the user runs `reset` or closes the window. That is screen exposure (A8, out of scope) plus a broken shell, not a disk leak. Every way to cause it from outside (`SIGKILL`, `SIGSTOP`, `kill -34`) needs a same-user process, which can already capture the screen (A6).

## Process hardening and sandboxing

Hardening happens in a fixed order: protections that cost nothing go first, unlock happens while exec is still possible (for hardware-key plugins), and the sandbox locks down before any note is decrypted or rendered. The detailed sandbox design (Landlock rules, seccomp policy, SELinux module, bubblewrap launcher, verification) is in docs/SANDBOXING.md.

1. Install the panic hook (restore terminal, print location only).
2. `setrlimit(RLIMIT_CORE, 0)`, `prctl(PR_SET_DUMPABLE, 0)`, `prctl(PR_SET_NO_NEW_PRIVS, 1)`, `umask(0o077)`. Then refuse to start if any fd above 2 is open (read from `/proc/self/fd`). Landlock does not restrict fds opened before it and seccomp allows `read` and `write`, so an inherited socket or directory fd would bypass the sandbox, and the age plugin would inherit it too. Crystal cannot close an fd it does not own without `unsafe` (rustix has no safe `close_range`), so it fails closed.
3. Environment allowlist: read only `HOME` (must be absolute; locates the config, state and default vault directories), `TERM`, `COLORTERM` and locale variables, through `harden::env_var`, which takes an `EnvVar` enum with one variant per allowed variable; never read `EDITOR`, `PAGER`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME` or similar. Clippy's `disallowed-methods` (clippy.toml) makes any other `std::env` read a build error. The process environment is not modified (`remove_var` is `unsafe` in edition 2024 and the crate forbids `unsafe`); instead, the only child process, the age plugin, is spawned with `env_clear()` plus the allowlist.
4. `mlockall` with `MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT`, after raising the soft `RLIMIT_MEMLOCK` to the hard limit. Refuse to continue if the hard limit is below 256 MiB (a smaller budget can run out mid-session, and the failed allocation aborts without zeroizing or saving) or if `mlockall` fails, with a message pointing to docs/SETUP.md.
5. Require a real tty; open `/dev/tty` directly for input and output.
6. Parse args in the `cli` module: one vault name (1 to 64 characters from `A-Z a-z 0-9 - _`, starting with a letter or digit, so no `/`, `..`, hidden names or option look-alikes), `--init` (with optional `--at DIR`), `--add NAME DIR`, `--remove NAME`, `--lock-after MINUTES` (default from the config, else 5; range 1 to 1440), `-h`/`--help`; from Phase 6 also `--sandbox-status`. Anything else is refused. Errors are fixed messages that never echo an argument, since a mistyped argument could be a passphrase; lexopt's own errors are discarded because they quote the argument.
7. Open `~/.config/crystal` and the state directory as directory fds with `O_DIRECTORY` and `O_NOFOLLOW`, and check that each is ours and mode 0700. The state directory is reached one component at a time from `HOME` with `O_NOFOLLOW`; `.local` and `state` must be ours and not group- or world-writable. The config directory is created (0700) only by `--init` or `--add`, never as a side effect of opening. Then enter raw mode and the alternate screen on the `/dev/tty` fd (terminal guard), so the passphrase prompt is drawn there.
8. Unlock: decrypt `config.age` with a passphrase or an age plugin, and reject it if its generation is older than `config.gen`. Look up the vault name, and open the vault path with the checks under **Config file** (the vault directory itself must be ours and mode 0700, even inside `~/Vaults`). Decrypt that vault's `identity.age` (same passphrase, or another plugin touch). Close the config fd. The vault and state fds are later reused as the Landlock rule fds, so other vaults and the config stay unreachable during a session. Plugins are external binaries, so this is the last moment `execve` is allowed.
9. Apply Landlock (docs/SANDBOXING.md section 5) through the `landlock` crate, up to ABI 9: read and write only beneath the vault, state and external backup directories (never the config directory); deny TCP bind and connect; block pathname Unix sockets (`RESOLVE_UNIX`); scope signals and abstract Unix sockets. The ruleset uses the crate's hard-requirement mode, so a right the kernel cannot enforce is an error. Below ABI 9, or without Landlock, Crystal refuses to start; there is no weaker mode and no override flag.
10. Apply seccomp (allowlist, below; docs/SANDBOXING.md section 6), with kill-process as the default action. The log action exists only in builds with the `sandbox-audit` feature, never in release.
11. Decrypt the vault, build the index, enter the TUI.

**Seccomp allowlist (starting point):** `read`, `write`, `openat`, `close`, `fstat`, `newfstatat`, `statx`, `lseek`, `getdents64`, `mkdirat`, `renameat2`, `unlinkat`, `fsync`, `fdatasync`, `mmap`, `munmap`, `mremap`, `mprotect` (deny `PROT_EXEC`), `brk`, `madvise`, `rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `sigaltstack`, `ioctl` (only `TCGETS2`, `TCSETS2`, `TCSETSF2`, `TIOCGWINSZ`: rustix uses the `*2` termios requests and falls back to `TCGETS` only on an error return, which a seccomp kill never gives; see docs/SYSCALLS.md), `poll`, `ppoll`, `epoll_*`, `clock_gettime`, `clock_nanosleep`, `futex`, `getrandom`, `getpid`, `kill` (own pid only, `SIGSTOP`, to suspend on Ctrl+Z or `SIGTSTP`), `exit`, `exit_group`.

**Explicitly denied:** `socket`, `socketpair`, `connect`, `bind`, `execve`, `execveat`, `ptrace`, `process_vm_readv`, `process_vm_writev`, `bpf`, `io_uring_setup`, `io_uring_enter`, `io_uring_register`, `userfaultfd`, `perf_event_open`, `keyctl`, `add_key`, `mount`, `unshare`, `setns`, `personality`, `memfd_create`, `clone`, `clone3` (Crystal is single-threaded), and the `ioctl` requests `TIOCSTI` and `TIOCLINUX`.

Develop with the `sandbox-audit` feature (log action), run every feature, and collect the real syscall set from the audit log with `run0 ausearch -m SECCOMP -c crystal`; this works with secureblue's `deny_ptrace` on. `strace -f -c` also works where ptrace is allowed. Then switch to kill. Crystal is single-threaded: if a dependency starts a thread, that is a finding to fix, not a reason to allow `clone`.

**System-level recommendations:** encrypted swap or zram only, hibernation disabled, Yama `ptrace_scope` at 1 or higher, full-disk encryption, and a terminal emulator without disk-backed scrollback.

## Installation

Crystal is a per-user executable at `~/.local/bin/crystal`. Installing or updating it needs no root, and nothing goes into `/usr/local` (`/var/usrlocal` on secureblue) or the OS image.

```sh
cargo build --locked --release
install -m 0555 target/x86_64-unknown-linux-musl/release/crystal ~/.local/bin/crystal
```

**What this gives up.** Every process running as you can write to `~/.local/bin`, so same-user malware (A5) can replace or shadow the binary. Every protection inside Crystal would then protect the fake, and the fake can ask for your passphrase. A passphrase phished this way works forever; with a hardware key, a fake gets only the unlocks you confirm by touch.

**Optional immutability.** `run0 chattr +i ~/.local/bin/crystal`. Setting or clearing the flag needs `CAP_LINUX_IMMUTABLE`, so no process running as you can overwrite, delete, rename or replace the file. To update, run `run0 chattr -i`, then `install` and `restorecon`, then `run0 chattr +i`; `install` fails on an immutable target, which is the point, and the label must be set before the flag, since an immutable file cannot be relabeled. The flag does not stop the parent directory from being renamed and recreated (`mv ~/.local/bin ~/.local/bin.old; mkdir ~/.local/bin`). For the full guarantee, also run `run0 chattr +i ~/.local/bin`, which blocks every other per-user install into that directory too.

**Shadowing.** Directories earlier in `PATH` (such as `~/.cargo/bin`) and shell startup files can still run a different `crystal`. Use an absolute-path alias (`alias crystal="$HOME/.local/bin/crystal"`) in a `~/.bashrc` that is itself immutable.

**No self-check.** Crystal does not check its own immutable flag or hash at startup: a replaced binary would simply skip the check, so it would prove nothing.

**SELinux.** The policy labels `~/.local/bin/crystal` as `crystal_exec_t`. A new install creates a new file with the default label, so relabel after every install (docs/SANDBOXING.md); otherwise Crystal runs outside `crystal_t`.

## Backups

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

## Phased build plan

Nine phases, each ending in a commit with passing acceptance tests; security layers (hardening, crypto, integrity) come before features, and the sandbox goes in once the syscall set is stable.

1. **Phase 0: Skeleton and policy.** Workspace with the `crystal` crate (modules `harden`, `term`, `cli`, `crypto`, `vault`, `backup`, `model`, `md`, `ui`, `sys`) and the `zalloc` allocator crate. Release profile, musl target, `#![forbid(unsafe_code)]`, `deny.toml`, CI running clippy with warnings denied, tests, `cargo deny check`, `cargo audit`.
   - Accept: `file crystal` reports a static binary; CI is green.
2. **Phase 1: Hardening bootstrap and terminal guard.** Steps 1 to 7 of the startup order; alt screen, raw mode, panic hook, signal handling.
   - Accept: `gdb -p` fails; `VmLck` is non-zero; a forced panic leaves a clean, cleared terminal.
3. **Phase 2: Crypto, vault format and config.** `crystal --init` (including `identity.backup.age` for the offline backup recipient), `--add`, `--remove`, unlock, `identity.age`, encrypted `config.age`, single vault blob, Padmé padding, atomic writer, generation counters for vaults and config in the state directory.
   - Accept: round-trip property tests; tamper suite refuses foreign, truncated, bit-flipped and rolled-back vaults and configs; a config pointing at a symlinked path, a group-writable parent, `$HOME`, `~/.ssh` (no `VERSION`), or the config or state directory is refused.
4. **Phase 3: Read-only viewer.** Fuzzy finder over titles, renderer and sanitizer, wikilink navigation, backlinks pane.
   - Accept: the escape-injection corpus renders as inert text; renderer fuzzing runs clean for an hour.
5. **Phase 4: Editor.** `tui-textarea` integration, save, new, rename, delete, Syncthing conflict detection with per-note merge.
   - Accept: `strace` shows only ciphertext writes inside the vault; kill -9 during save never corrupts the vault.
6. **Phase 5: Memory hygiene and comfort.** Zeroizing allocator, auto-lock, lock on signals, full-text search, tags.
   - Accept: the canary scan finds no plaintext after lock.
7. **Phase 6: Sandbox.** Landlock and seccomp in log mode, collect syscalls, then switch to kill. SELinux policy module and optional bubblewrap launcher. Debug-only `--sandbox-selftest CHECK` and `--sandbox-status` (docs/SANDBOXING.md).
   - Accept: the checks in docs/SANDBOXING.md section 9 pass, including that `socket`, `execve` and opening `~/.ssh` are killed or denied.
8. **Phase 7: Backups.** Local snapshot store, external drive marker, headless mode with udev trigger, retention, verify, restore.
   - Accept: delete the vault, restore from the drive, every note matches; a tampered snapshot is refused; a headless run copies a new vault.age without the key.
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), passphrase change and key rotation, explicit plaintext export with a warning, reproducible builds, signed releases, embedded dependency list via `cargo auditable`, per-user install to `~/.local/bin` with the optional immutable flag (Installation).
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
| Sandbox checks | Denied syscalls kill the process; paths outside the vault return EACCES | Child-process tests through the debug-only `--sandbox-selftest CHECK`, one check per process (docs/SANDBOXING.md section 9) |
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
- Never write files except through vault::atomic_write or backup::store, and only
  ciphertext, or the fixed public formats `VERSION` and `*.gen` (`PublicBytes`).
- Never spawn processes, except the age plugin during unlock, before the sandbox.
- Any new syscall needs a line in docs/SYSCALLS.md explaining why.
- Never weaken Landlock, seccomp or hardening to make a test pass.

## Before finishing a task
- cargo fmt --all --check
- cargo clippy --all-targets -- -D warnings
- cargo test
- cargo deny check
- cargo audit
- List what changed and which acceptance criteria now pass.
```

**Prompts per phase** (one session each):

1. Phase 0: "Scaffold the crate per DESIGN.md Phase 0. Set up the release profile, musl target, deny.toml and CI. No features yet."
2. Phase 1: "Implement the startup order steps 1 to 7 and the terminal guard. Add tests for every Phase 1 acceptance criterion."
3. Phase 2: "Implement the vault format exactly as specified in the Architecture section. Write the tamper suite first, then the code."
4. Phase 3 onward: "Implement Phase N per DESIGN.md. Start by listing the acceptance tests you will write."

After each phase, ask a fresh session to review the diff as an attacker: "Review this diff against the threat model. Find any path where plaintext can reach disk, the terminal outside the alt screen, an error message, or another process."
