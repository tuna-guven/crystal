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
| *(none yet)* | | | |

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
| `openat` | `vault::VaultsRoot::open` | Open `~/Vaults` once, only to open one vault inside it; closed right after | `AT_FDCWD`, `~/Vaults`; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `fstat` | `vault::VaultsRoot::open` | Check owner and mode of `~/Vaults` on the open fd | `~/Vaults` fd |
| `openat` | `vault::VaultDir::open` | Open the vault relative to the root fd; later file operations are relative to this fd | `~/Vaults` fd, validated vault name from argv; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `fstat` | `vault::VaultDir::open` | Check the vault directory is ours and mode 0700, not only `~/Vaults` around it | vault fd |
| `openat` | `vault::StateDir::open_or_create` | Open `HOME`, the start of a walk one directory at a time, so no path is resolved again later; `HOME` itself may contain symlinks (`/home` is one on Fedora Atomic) | `AT_FDCWD`, `HOME`; flags `O_RDONLY`, `O_DIRECTORY`, `O_CLOEXEC` |
| `mkdirat` | `vault::StateDir::open_or_create` | Create a missing `.local`, `state` or `crystal`, each relative to its parent's fd | parent fd, one name; mode 0700 |
| `openat` | `vault::StateDir::open_or_create` | Open `.local`, `state`, then `crystal`, each relative to its parent's fd; `O_NOFOLLOW` refuses a symlink at any level | parent fd, one name; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `fstat` | `vault::StateDir::open_or_create` | `.local` and `state` must be ours and not group- or world-writable; `crystal` must be ours and mode 0700. Checked on the open fd, not the path | each opened fd |
| `geteuid` | `vault::StateDir::open_or_create` | Compare the state directory owner with our uid | none |
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
| `close` | `Drop` of `term::Tty`, `vault::VaultsRoot`, `vault::VaultDir`, `vault::StateDir` (and the `HOME`, `.local` and `state` fds of its walk), `term::TerminalGuard` | Release the terminal and directory fds | none |

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
