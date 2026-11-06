# Syscalls

Every syscall Crystal needs after the sandbox is applied, and why. The
seccomp allowlist (DESIGN.md, "Process hardening and sandboxing") must match
this table exactly: no syscall is allowed without a row here.

Add a row in the same change that first needs the syscall. Arguments that are
restricted by the filter (for example `ioctl` request numbers) go in the
"Restriction" column. Keep each table free of blank lines (a blank line ends
a Markdown table) and write flag combinations as comma-separated lists, not
with `|`, which splits cells in many renderers.

| Syscall | Used by | Why | Restriction |
| --- | --- | --- | --- |
| `getdents64` | `vault::atomic_write` (sweep, rustix `Dir`) | List the target's directory to find `.<name>.tmp-<16 hex>` files a killed writer left behind | fd for `.` in the target's directory |
| `openat` | `vault::atomic_write` (sweep) | A separate fd for `.` to list the directory, so the caller's directory fd keeps no read position | target's directory fd, `.`; flags `O_RDONLY`, `O_DIRECTORY`, `O_CLOEXEC` |
| `unlinkat` | `vault::atomic_write` | Remove stale temp files of the same name, and the temp file when a step before the rename fails | target's directory fd, a temp name only; flags 0 |
| `getrandom` | `crypto::fill_random`, from `vault::atomic_write` | 64 random bits for the temp file name | 8-byte buffer, flags 0 |
| `getrandom` | `crypto::encrypt_to_recipients` and `encrypt_to_passphrase`, through age's `OsRng` (`rand_core` 0.6, `getrandom` 0.2) | Every save: a new age file key, payload nonce and ephemeral X25519 key (and a scrypt salt for `identity.age`); `VaultIdentity::generate` for `--init`. `getrandom` 0.2 first probes once with a zero-length buffer; on musl, if that returns `ENOSYS` or `EPERM` it falls back to opening `/dev/urandom` (which Landlock denies), so the seccomp filter must allow this syscall, not return an errno for it | any buffer length including 0, flags 0 |
| `openat` | `vault::atomic_write` | Create the temp file in the target's own directory, so the rename needs no Landlock `REFER` (SANDBOXING.md 5.2); `O_EXCL` and `O_NOFOLLOW` never open an existing file or follow a link | target's directory fd, `.<name>.tmp-<16 hex>`; flags `O_WRONLY`, `O_CREAT`, `O_EXCL`, `O_NOFOLLOW`, `O_CLOEXEC`; mode 0600 |
| `write` | `vault::atomic_write` | Write the ciphertext (or `VERSION` / `*.gen`) to the temp file; retried on short writes and `EINTR` | temp file fd |
| `fsync` | `vault::atomic_write` | Temp file data on disk before the rename, so the rename never exposes a half-written file after a power loss | temp file fd |
| `renameat2` | `vault::atomic_write` (rustix `renameat_with`, which always uses `renameat2`; plain `renameat` is the old `renameat` syscall on x86_64) | Replace the target with the temp file: the commit point. A symlink at the target name is replaced, not followed | same directory fd for both sides; flags 0 |
| `fsync` | `vault::atomic_write` | Make the rename itself durable | target's directory fd |
| `close` | `vault::atomic_write` | Release the temp file and listing fds | none |

## Pre-sandbox only

Called during startup, before Landlock and seccomp. These are **not** in the
seccomp allowlist; a call after the sandbox is applied kills the process.
Names are the real syscalls, checked in rustix's linux_raw backend, not the
wrapper names.

| Syscall | Used by | Why | Arguments |
| --- | --- | --- | --- |
| `prlimit64` | `harden::lock_down_process` (rustix `setrlimit`) | `RLIMIT_CORE` = 0 soft and hard: no core dumps | pid 0 (self), `RLIMIT_CORE` |
| `prctl` | `harden::lock_down_process` | `PR_SET_DUMPABLE` 0: blocks ptrace and /proc/pid/mem from same-user processes | `PR_SET_DUMPABLE`, 0 |
| `prctl` | `harden::lock_down_process` | `PR_SET_NO_NEW_PRIVS` 1: exec can't gain privileges; required for seccomp | `PR_SET_NO_NEW_PRIVS`, 1 |
| `umask` | `harden::lock_down_process` | New files are owner-only (0o077) | 0o077 |
| `openat` | `harden::refuse_inherited_fds` | List open fds, to refuse any inherited fd above 2: Landlock does not restrict fds opened before it, and closing an fd Crystal does not own needs `unsafe` | `AT_FDCWD`, `/proc/self/fd`; flags `O_RDONLY`, `O_DIRECTORY`, `O_CLOEXEC` |
| `getdents64` | `harden::refuse_inherited_fds` (rustix `Dir`) | Read the fd numbers | `/proc/self/fd` fd |
| `close` | `harden::refuse_inherited_fds` | Release the `/proc/self/fd` fd before any other fd is opened | `/proc/self/fd` fd |
| `prlimit64` | `harden::lock_memory` (rustix `getrlimit`) | Read the memlock hard limit | pid 0 (self), `RLIMIT_MEMLOCK`, no new value |
| `prlimit64` | `harden::lock_memory` (rustix `setrlimit`) | Raise the memlock soft limit to the hard limit | pid 0 (self), `RLIMIT_MEMLOCK` |
| `mlockall` | `harden::lock_memory` | Keep all memory out of swap and hibernation | flags `MCL_CURRENT`, `MCL_FUTURE`, `MCL_ONFAULT` |
| `openat` | `term::Tty::open` | Open the controlling terminal directly instead of trusting stdin/stdout | `AT_FDCWD`, `/dev/tty`; flags `O_RDWR`, `O_CLOEXEC`, `O_NOCTTY`, `O_NOFOLLOW` |
| `openat` | `vault::open_home_subdir`, from `ConfigDir::open` (then `StateDir::open_or_create`) | Open `HOME`, the start of a walk one directory at a time, so no path is resolved again later; `HOME` itself may contain symlinks (`/home` is one on Fedora Atomic) | `AT_FDCWD`, `HOME`; flags `O_RDONLY`, `O_DIRECTORY`, `O_CLOEXEC` |
| `openat` | `vault::open_home_subdir` | Open `.config` then `crystal` (config), or `.local`, `state`, then `crystal` (state), each relative to its parent's fd; `O_NOFOLLOW` refuses a symlink at any level | parent fd, one name; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `mkdirat` | `vault::open_home_subdir`, from `StateDir::open_or_create` only (`ConfigDir::create` runs only for `--init`/`--add`, which exit before step 8) | Create a missing `.local`, `state` or `crystal`, each relative to its parent's fd. A normal run never creates the config directory | parent fd, one name; mode 0700 |
| `fstat` | `vault::open_home_subdir` | Parents (`.config`, `.local`, `state`) must be ours and not group- or world-writable; each `crystal` must be ours and mode 0700. Checked on the open fd, not the path | each opened fd |
| `geteuid` | `vault::open_home_subdir` | Compare each directory's owner with our uid | none |
| `getrandom` | `crypto::fill_random` (rustix `rand::getrandom`), from `model::NoteId::random` and `model::VaultId::random` | Random ids for new notes and vaults from the kernel's secure generator; no flags, so it blocks until the pool is seeded instead of returning weak bytes, and needs no `/dev/urandom` file access. Not called by a normal run until #10/#14; already on the planned seccomp allowlist (DESIGN.md) | buffer of 16 or 32 bytes, flags 0 |
| `ioctl` | `term::Tty::open` (rustix `isatty`) | Confirm `/dev/tty` is a terminal | `TIOCGWINSZ` only |
| `fcntl` | `term::TerminalGuard::enter` (std `File::try_clone`) | Second terminal fd, kept for `term::restore_terminal` so the panic hook can restore the terminal | `F_DUPFD_CLOEXEC` |
| `ioctl` | `term::TerminalGuard::enter` (rustix `tcgetattr`) | Save the terminal settings to restore on exit | `TCGETS2` (rustix falls back to `TCGETS` only if `TCGETS2` returns an error) |
| `ioctl` | `term::TerminalGuard::enter` (rustix `tcsetattr`, `Now`) | Raw mode: no echo, no line buffering, no signals from keys | `TCSETS2` |
| `ioctl` | `term::restore_terminal`, from the guard's `Drop` or the panic hook (rustix `tcsetattr`, `Flush`) | Restore the saved settings and discard unread input, so keys typed into Crystal never reach the shell | `TCSETSF2` |
| `pipe2` | `term::Signals::register` (rustix `pipe_with`) | Two self-pipes: one for exit signals, one for `SIGTSTP` | `O_CLOEXEC`, `O_NONBLOCK` |
| `fcntl` | `term::Signals::register` (`OwnedFd::try_clone`, and inside signal-hook's `pipe::register`) | One write-end copy per signal (about 45 fds, all close-on-exec); signal-hook sets `FD_CLOEXEC` and `O_NONBLOCK` on it | `F_DUPFD_CLOEXEC`, `F_GETFD`, `F_SETFD`, `F_GETFL`, `F_SETFL` |
| `sendto` | signal-hook `pipe::register` | One zero-byte probe to test whether the fd is a socket; on our pipe it fails with `ENOTSOCK`, and signal-hook uses `write` instead | pipe write end, 0 bytes, `MSG_DONTWAIT` |
| `rt_sigaction` | signal-hook `pipe::register` | Install handlers for every catchable signal whose default kills the process (`term::EXIT_SIGNALS` and real-time signals), so each ends Crystal through the normal exit path; and `SIGTSTP` (suspend) | `SIGTERM`, `SIGHUP`, `SIGINT`, `SIGQUIT`, `SIGUSR1`, `SIGUSR2`, `SIGALRM`, `SIGVTALRM`, `SIGPROF`, `SIGXCPU`, `SIGXFSZ`, `SIGIO`, `SIGPWR`, `SIGSTKFLT`, signals 32 to 64 (musl refuses 32 to 34 with `EINVAL`; those stay uncatchable), `SIGTSTP` |
| `ppoll` | `term::TerminalGuard::run_until_exit` (rustix `poll`) | Wait for a key, an exit signal or a suspend | `/dev/tty` fd and both signal pipe read ends, no timeout |
| `read` | `term::TerminalGuard::run_until_exit` | Read and discard keys (Ctrl+Z suspends, any other key exits) | `/dev/tty` fd |
| `read` | `term::Signals::drain_suspend` | Empty the suspend pipe so one `SIGTSTP` suspends once | suspend pipe read end, non-blocking |
| `getpid` | `term::TerminalGuard::suspend` | Own pid, to stop ourselves | none |
| `kill` | `term::TerminalGuard::suspend` (rustix `kill_process`) | Stop ourselves after restoring the terminal; returns once `fg`/`bg` sends `SIGCONT` | own pid only, `SIGSTOP` |
| `write` | exit-signal handler (signal-hook) | Wake the main loop: one byte to the signal pipe (async-signal-safe) | signal pipe write end |
| `rt_sigreturn` | kernel, after any signal handler | Return from the handler | none |
| `write` | `main`, panic hook | Usage text on `--help` (fd 1); error and panic messages (fd 2): fixed text, errnos and source locations only | fd 1, fd 2 |
| `write` | `term::TerminalGuard::enter`, `term::restore_terminal` | Enter, clear and leave the alternate screen | `/dev/tty` fds |
| `close` | `Drop` of `term::Tty`, `vault::ConfigDir`, `vault::StateDir` (and the `HOME`, `.config`, `.local` and `state` fds of their walks), `term::TerminalGuard` | Release the terminal and directory fds | none |

With `MCL_FUTURE`, later `mmap` and `brk` calls are locked by the kernel
automatically; locking adds no syscalls after the sandbox.

`openat`, `ioctl(TIOCGWINSZ)`, `write` and `close` will also be needed after
the sandbox (TUI drawing, resize); they move to the allowlist table when that
code exists. A signal can arrive at any time, so `write` to the signal pipes and
`rt_sigreturn` must be in the allowlist; the TUI loop also needs `ppoll`,
`read` on `/dev/tty` and the suspend pipe, and, for suspend, `getpid` and
`kill` (own pid, `SIGSTOP`) plus everything the terminal guard uses to
re-engage on resume (`fcntl`, `ioctl` `TCGETS2` and `TCSETS2`, `write`). Only signal-hook's `low_level::pipe` is used: its
`iterator` module creates a `socketpair` and its `channel` module spawns a
thread, both denied. `term::restore_terminal` runs at exit or on a panic, after the sandbox, so
`ioctl(TCSETSF2)` and `write` to `/dev/tty` must be in the allowlist too. The
seccomp filter must allow the `*2` termios requests rustix actually uses:
its fallback to `TCGETS` happens only on an error return, and a seccomp kill
never returns. Syscalls made by the Rust runtime and musl before `main` are not
listed here; Phase 6 collects them in seccomp log mode.
