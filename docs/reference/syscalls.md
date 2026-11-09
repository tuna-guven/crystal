# Syscalls

Every syscall Crystal needs after the sandbox is applied, and why. The
seccomp allowlist ([Startup order and process hardening](../design/startup.md)) must match
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
| `getrandom` | `crypto::fill_random` (rustix `rand::getrandom`), from Crystal's age code (`age_v1`, [decision 0042](../decisions/0042-own-age-implementation.md)) under `crypto::encrypt_to_recipients`, `encrypt_to_passphrase` and `VaultIdentity::generate` | Every save: a new age file key, payload nonce and ephemeral X25519 key (and a scrypt salt for `identity.age` and `identity.backup.age`); the vault's X25519 identity for `crystal init`. No flags, so it blocks until the pool is seeded, and no `/dev/urandom` file access. Since step C2 the binary no longer contains age's `OsRng` (`getrandom` 0.2) and its zero-length probe; the seccomp filter still allows this syscall and never answers it with an errno (decision 0042) | buffer of 16 or 32 bytes, flags 0 |
| `openat` | `vault::atomic_write` | Create the temp file in the target's own directory, so the rename needs no Landlock `REFER` (sandboxing 5.2); `O_EXCL` and `O_NOFOLLOW` never open an existing file or follow a link | target's directory fd, `.<name>.tmp-<16 hex>`; flags `O_WRONLY`, `O_CREAT`, `O_EXCL`, `O_NOFOLLOW`, `O_CLOEXEC`; mode 0600 |
| `write` | `vault::atomic_write` | Write the ciphertext (or `VERSION` / `*.gen`) to the temp file; retried on short writes and `EINTR` | temp file fd |
| `fsync` | `vault::atomic_write` | Temp file data on disk before the rename, so the rename never exposes a half-written file after a power loss | temp file fd |
| `renameat2` | `vault::atomic_write` (rustix `renameat_with`, which always uses `renameat2`; plain `renameat` is the old `renameat` syscall on x86_64) | Replace the target with the temp file: the commit point. A symlink at the target name is replaced, not followed | same directory fd for both sides; flags 0 |
| `fsync` | `vault::atomic_write` | Make the rename itself durable | target's directory fd |
| `close` | `vault::atomic_write` | Release the temp file and listing fds | none |
| `openat` | `vault::generation::load` (`public_file::read_bounded`), on every save and at unlock | Open the vault's `.gen` counter; `O_NOFOLLOW` refuses a symlink, `O_NONBLOCK` keeps a FIFO from blocking the open. A missing file (`ENOENT`) means first use | state directory fd, `<vault_id hex>.gen` or `config.gen`; flags `O_RDONLY`, `O_NOFOLLOW`, `O_NONBLOCK`, `O_CLOEXEC` |
| `fstat` | `vault::generation::load` | Refuse anything but a regular file | the counter's fd |
| `read` | `vault::generation::load` | Read at most 22 bytes (the longest valid counter is 21) | the counter's fd |
| `close` | `vault::generation::load` | Release the counter's fd | none |

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
| `openat` | `vault::open_home_subdir` | Open `.local`, `share` then `crystal` (vaults, `DataDir`), or `.local`, `state` then `crystal` (state), each relative to its parent's fd; `O_NOFOLLOW` refuses a symlink at any level | parent fd, one name; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `mkdirat` | `vault::open_home_subdir`, from `StateDir::open_or_create`, and `DataDir::create` for `crystal init` only (which exits before step 8) | Create a missing `.local`, `share`, `state` or `crystal`, each relative to its parent's fd. A normal run never creates the vault directory `~/.local/share/crystal` | parent fd, one name; mode 0700 |
| `fstat` | `vault::open_home_subdir` | Parents (`.local`, `share`, `state`) must be ours and not group- or world-writable; each `crystal` must be ours and mode 0700. Checked on the open fd, not the path | each opened fd |
| `openat` | `vault::format::read_version`, from `VaultDir::open` (unlock, `crystal recover`, `check-code`, and `init`'s final check) | Open `VERSION` before anything is decrypted; `O_NOFOLLOW` refuses a symlink, `O_NONBLOCK` keeps a FIFO from blocking the open | vault directory fd, `VERSION`; flags `O_RDONLY`, `O_NOFOLLOW`, `O_NONBLOCK`, `O_CLOEXEC` |
| `fstat` | `vault::format::read_version` | Refuse anything but a regular file | the `VERSION` fd |
| `read` | `vault::format::read_version` | Read at most 7 bytes (the longest valid content is 6) | the `VERSION` fd |
| `close` | `vault::format::read_version` | Release the `VERSION` fd | none |
| `geteuid` | `vault::open_home_subdir` | Compare each directory's owner with our uid | none |
| `openat` | `vault::VaultDir::open` (and `open_for_removal` for `crystal rm`), before any prompt | Open the vault directory `NAME` relative to the `~/.local/share/crystal` fd; `O_NOFOLLOW` refuses a symlink | data directory fd, the vault name; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `fstat` | `vault::VaultDir::open` | The vault directory must be ours and mode 0700 | the vault directory fd |
| `geteuid` | `vault::VaultDir::open` | Compare the owner with our uid | none |
| `newfstatat` | `init::plan` (rustix `statat`, `AT_SYMLINK_NOFOLLOW`) | Refuse an existing vault (or a symlink at its name) before anything is asked or created | data directory fd, the vault name |
| `prlimit64` | `crypto::scrypt_policy` (rustix `getrlimit`), once per run, before the first passphrase is used (`crystal init`, `recover`, `check-code`, unlock) | Read the `RLIMIT_MEMLOCK` hard limit, so a passphrase file is never decrypted with more scrypt memory than half of the locked-memory budget | pid 0 (self), `RLIMIT_MEMLOCK`, no new value |
| `mkdirat` | `init::create` | Create the vault directory in `~/.local/share/crystal` | data directory fd, the vault name; mode 0700 |
| `openat` | `init::create` | Open the new vault directory to write into it | data directory fd, the vault name; flags `O_RDONLY`, `O_DIRECTORY`, `O_NOFOLLOW`, `O_CLOEXEC` |
| `unlinkat` | `init::create`, on failure only | Remove the files, the vault directory and the pin this run created; never anything that existed before | vault directory fd and its four file names; data directory fd and the vault name with `AT_REMOVEDIR`; state directory fd and `NAME.pin` |
| `openat`, `getdents64` | `rm::remove` (rustix `Dir`), for `crystal rm` only, before the sandbox | List the vault directory to find Crystal's own files | vault directory fd, `.`; flags `O_RDONLY`, `O_DIRECTORY`, `O_CLOEXEC` |
| `unlinkat` | `rm::remove`, `vault::pin::remove`, `generation::remove`, for `crystal rm` only, before the sandbox; decision 0032 | Delete the vault's own files (the four vault files, sync conflict copies, atomic-writer temp files), then the directory if nothing else is in it, then its `.gen` and `.pin` | vault directory fd and those names; data directory fd and the vault name with `AT_REMOVEDIR`; state directory fd and the two state files |
| `openat` | `public_file::read_bounded`, from `unlock::unlock`, `recover::open_backup`, `recover::check_code`, `vault::pin::load` and `vault::generation::load` | Read a vault's `identity.age`, `identity.backup.age` and `vault.age`, and the state directory's `.pin` and `.gen` files. Regular files only, with a size limit | vault or state directory fd, the file name; flags `O_RDONLY`, `O_NOFOLLOW`, `O_NONBLOCK`, `O_CLOEXEC` |
| `fstat` | `public_file::read_bounded` | Refuse anything but a regular file; size the buffer once | the file's fd |
| `read` | `public_file::read_bounded` | Read up to the limit plus one byte | the file's fd |
| `close` | `public_file::read_bounded` | Release the file's fd | none |
| `openat` | `unlock::has_sync_conflict`, at unlock | A separate fd for `.` to list the vault directory | vault directory fd, `.`; flags `O_RDONLY`, `O_DIRECTORY`, `O_CLOEXEC` |
| `getdents64` | `unlock::has_sync_conflict` (rustix `Dir`) | Refuse a `vault.sync-conflict-*.age` until the merge exists (Phase 4) | the listing fd |
| `getrandom` | `crypto::fill_random` (rustix `rand::getrandom`), from `model::NoteId::random`, `model::VaultId::random` and `recovery::RecoveryCode::generate` | Random ids for new notes and vaults, and the 135 random bits of a vault's recovery code (`crystal init`), from the kernel's secure generator; no flags, so it blocks until the pool is seeded instead of returning weak bytes, and needs no `/dev/urandom` file access. Not called by a normal run until #10/#14; already on the planned seccomp allowlist (startup chapter) | buffer of 16, 17 or 32 bytes, flags 0 |
| `ioctl` | `term::Tty::open` (rustix `isatty`) | Confirm `/dev/tty` is a terminal | `TIOCGWINSZ` only |
| `fcntl` | `term::TerminalGuard::enter` (std `File::try_clone`) | Second terminal fd, kept for `term::restore_terminal` so the panic hook can restore the terminal | `F_DUPFD_CLOEXEC` |
| `ioctl` | `term::TerminalGuard::enter` (rustix `tcgetattr`) | Save the terminal settings to restore on exit | `TCGETS2` (rustix falls back to `TCGETS` only if `TCGETS2` returns an error) |
| `ioctl` | `term::TerminalGuard::enter` (rustix `tcsetattr`, `Now`) | Raw mode: no echo, no line buffering, no signals from keys | `TCSETS2` |
| `ioctl` | `term::restore_terminal`, from the guard's `Drop` or the panic hook (rustix `tcsetattr`, `Flush`) | Restore the saved settings and discard unread input, so keys typed into Crystal never reach the shell | `TCSETSF2` |
| `pipe2` | `term::Signals::register` (rustix `pipe_with`) | Three self-pipes: one for exit signals, one for `SIGTSTP`, one for `SIGWINCH` | `O_CLOEXEC`, `O_NONBLOCK` |
| `fcntl` | `term::Signals::register` (`OwnedFd::try_clone`, and inside signal-hook's `pipe::register`) | One write-end copy per signal (about 45 fds, all close-on-exec); signal-hook sets `FD_CLOEXEC` and `O_NONBLOCK` on it | `F_DUPFD_CLOEXEC`, `F_GETFD`, `F_SETFD`, `F_GETFL`, `F_SETFL` |
| `sendto` | signal-hook `pipe::register` | One zero-byte probe to test whether the fd is a socket; on our pipe it fails with `ENOTSOCK`, and signal-hook uses `write` instead | pipe write end, 0 bytes, `MSG_DONTWAIT` |
| `rt_sigaction` | signal-hook `pipe::register` | Install handlers for every catchable signal whose default kills the process (`term::EXIT_SIGNALS` and real-time signals), so each ends Crystal through the normal exit path; `SIGTSTP` (suspend); and `SIGWINCH` (resize; its default is to ignore, the handler only wakes the loop) | `SIGTERM`, `SIGHUP`, `SIGINT`, `SIGQUIT`, `SIGUSR1`, `SIGUSR2`, `SIGALRM`, `SIGVTALRM`, `SIGPROF`, `SIGXCPU`, `SIGXFSZ`, `SIGIO`, `SIGPWR`, `SIGSTKFLT`, signals 32 to 64 (musl refuses 32 to 34 with `EINVAL`; those stay uncatchable), `SIGTSTP` |
| `ppoll` | `term::TerminalGuard::wait` (rustix `poll`), from the shell's loop (and the debug-only `run_until_exit`) | Wait for a key, an exit signal, a suspend or a resize; `wait` takes an optional timeout for auto-lock (Phase 5), so far always none | `/dev/tty` fd and the three signal pipe read ends, no timeout |
| `ioctl` | `term::TerminalGuard::read_passphrase` and `read_public_line` (rustix `tcflush`) | Discard keys typed before the prompt appeared: the terminal echoed them while it was not yet raw, so they must not become part of the passphrase. Unlock, `crystal init`, `add` and `recover` only, before the sandbox; if Phase 5's re-lock prompt runs inside the sandbox, `TCFLSH` must join the ioctl allowlist | `/dev/tty` fd, `TCFLSH`, `TCIFLUSH` |
| `ppoll` | `term::TerminalGuard::read_passphrase` and `read_public_line` (rustix `poll`) | Wait for a key or an exit signal (which cancels the prompt) | `/dev/tty` fd and the exit signal pipe read end, no timeout |
| `read` | `term::TerminalGuard::read_passphrase` and `read_public_line` | Read passphrase keys, up to 64 bytes at a time into a stack buffer that is wiped on drop | `/dev/tty` fd |
| `write` | `term::TerminalGuard::read_passphrase` and `read_public_line` | Clear the alternate screen and draw the prompt; a passphrase is never echoed, a public line (the recovery code, typed back at `crystal init` and `crystal recover`) echoes printable ASCII only | `/dev/tty` fd |
| `read` | `term::TerminalGuard::wait` | Read keys into the caller's buffer (Ctrl+Z suspends instead); `run_until_exit` discards them | `/dev/tty` fd |
| `read` | `term::drain`, from `TerminalGuard::wait` | Empty the suspend or resize pipe, so a burst of one signal is handled once and the loop does not spin | suspend or resize pipe read end, non-blocking |
| `ioctl` | `term::TerminalGuard::size` (rustix `tcgetwinsize`), from the viewer loop at start and after each resize or resume | Read the window size for layout; a size of zero falls back to 80 by 24 | `/dev/tty` fd, `TIOCGWINSZ` |
| `write` | `ui::screen::Screen::frame`, through `ui::frame::FrameWriter` | Draw a frame: changed cells, cursor moves and styles only, from a wiped 4 KiB chunk; every symbol is checked for control and BiDi characters first ([decision 0031](../decisions/0031-own-grid.md)) | `/dev/tty` fd |
| `getpid` | `term::TerminalGuard::suspend` | Own pid, to stop ourselves | none |
| `kill` | `term::TerminalGuard::suspend` (rustix `kill_process`) | Stop ourselves after restoring the terminal; returns once `fg`/`bg` sends `SIGCONT` | own pid only, `SIGSTOP` |
| `write` | exit-signal handler (signal-hook) | Wake the main loop: one byte to the signal pipe (async-signal-safe) | signal pipe write end |
| `rt_sigreturn` | kernel, after any signal handler | Return from the handler | none |
| `write` | `main`, panic hook | Usage text on `--help` (fd 1); error and panic messages (fd 2): fixed text, errnos and source locations only | fd 1, fd 2 |
| `write` | `term::TerminalGuard::enter`, `term::restore_terminal` | Enter, clear and leave the alternate screen | `/dev/tty` fds |
| `close` | `Drop` of `term::Tty`, `vault::DataDir`, `vault::StateDir`, `vault::vault_dir::VaultDir` (and the `HOME`, `.local`, `share` and `state` fds of their walks), `term::TerminalGuard` | Release the terminal and directory fds | none |

With `MCL_FUTURE`, later `mmap` and `brk` calls are locked by the kernel
automatically; locking adds no syscalls after the sandbox.

`openat`, `ioctl(TIOCGWINSZ)`, `write` and `close` will also be needed after
the sandbox (TUI drawing, resize); they move to the allowlist table when that
code exists. A signal can arrive at any time, so `write` to the signal pipes and
`rt_sigreturn` must be in the allowlist; the TUI loop also needs `ppoll`,
`read` on `/dev/tty` and the suspend and resize pipes, and, for suspend, `getpid` and
`kill` (own pid, `SIGSTOP`) plus everything the terminal guard uses to
re-engage on resume (`fcntl`, `ioctl` `TCGETS2` and `TCSETS2`, `write`). Only signal-hook's `low_level::pipe` is used: its
`iterator` module creates a `socketpair` and its `channel` module spawns a
thread, both denied. `term::restore_terminal` runs at exit or on a panic, after the sandbox, so
`ioctl(TCSETSF2)` and `write` to `/dev/tty` must be in the allowlist too. The
seccomp filter must allow the `*2` termios requests rustix actually uses:
its fallback to `TCGETS` happens only on an error return, and a seccomp kill
never returns. Syscalls made by the Rust runtime and musl before `main` are not
listed here; Phase 6 collects them in seccomp log mode.
