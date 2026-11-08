# Plaintext leakage prevention

Every path plaintext could take out of the process is listed below with its mitigation and a concrete way to verify it.

| Leak path | Mitigation | How to verify |
| --- | --- | --- |
| Editor temp, swap, undo files | In-process editor only; never spawn `$EDITOR` | `strace -f -e trace=openat,write` shows writes only to `*.age` and temp ciphertext |
| Stale heap copies after realloc | Zeroizing global allocator; `Zeroizing` buffers for all plaintext | Canary scan test ([Testing](../development/testing.md)) |
| Copies of the vault key inside age | The vault identity crosses age's API as its text form (`AGE-SECRET-KEY-1…`): `VaultIdentity::seal_with_passphrase` and `seal_to_recipients` call age's `to_string`, and unlock parses the decrypted text back with `from_str`. Crystal holds the results only in `SecretString` or `Zeroizing` buffers, but age's bech32 encoding and decoding make intermediate copies that are freed without being wiped. Until the zeroizing global allocator (Phase 5) wipes every freed block, those copies can stay in the heap, in locked memory, until the process exits: never in swap (`mlockall`), a core dump or `/proc/pid/mem` (non-dumpable). Reachable only by someone who can already read Crystal's memory (A6) | Canary scan test for the key string once the allocator is in place (Phase 5) |
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

## Exits that skip cleanup

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
- seccomp kill: killing at once, with no code running afterwards, is the point ([Sandboxing](sandboxing.md)).

**What still holds on every one of these paths**

- Nothing is printed: no exit path prints a panic message (`tests/startup.rs`, `abort_paths_print_no_secret_and_leave_no_core`).
- No core dump: `RLIMIT_CORE` is 0 (hard limit too) and the process is non-dumpable from the first lines of `main`.
- No swap: memory is `mlockall`ed. The kernel frees the pages and zeroes them before any other process gets them (on secureblue, `init_on_free=1` zeroes them at once).
- Nothing on disk: Crystal never writes plaintext, so a kill can only leave ciphertext (`kill_9_leaves_no_new_files`; from Phase 4, also during a save).

**Residual risk:** the last frame stays on the alternate screen and the terminal stays raw until the user runs `reset` or closes the window. That is screen exposure (A8, out of scope) plus a broken shell, not a disk leak. Every way to cause it from outside (`SIGKILL`, `SIGSTOP`, `kill -34`) needs a same-user process, which can already capture the screen (A6).
