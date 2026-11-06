# Setup

System settings Crystal needs, and how to develop on a hardened host.
Crystal checks these at startup and refuses to run if they are missing.

Root commands below use `run0` (systemd, authorized through polkit) instead
of `sudo`: secureblue ships no SUID `sudo`, which removes a whole class of
SUID privilege-escalation bugs. On other distributions `sudo` works the same.

## Installing

Crystal is installed root-owned in `/usr/local/bin`, so only `run0` can
replace it. A binary in `~/.local/bin` could be swapped silently by any
program running as you, and every protection inside Crystal would then
protect the fake. On secureblue `/usr/local` is `/var/usrlocal`: mutable
machine state, outside the verified OS image, so installing there never
touches the image or a UKI.

```sh
cargo build --locked --release
run0 install -m 0755 -o root -g root \
  target/x86_64-unknown-linux-musl/release/crystal /usr/local/bin/crystal
run0 restorecon /usr/local/bin/crystal    # after every install, once the SELinux module exists
```

**Add an absolute-path alias** to `~/.bashrc`:

```bash
alias crystal=/usr/local/bin/crystal
```

Why: `~/.cargo/bin` and `~/.local/bin` are writable by any program running
as you and come before `/usr/local/bin` in `PATH`, so a planted
`crystal` there would run instead of the real one. Bash checks aliases
before `PATH`. This only protects you if `~/.bashrc` itself cannot be
changed without root (on this machine it carries the immutable flag;
check with `lsattr ~/.bashrc`).

## Vaults

Every vault lives in `~/Vaults/<name>` and is opened by name:
`crystal diary`. Names are 1 to 64 characters from `A-Z a-z 0-9 - _`,
starting with a letter or digit. `~/Vaults` must be yours and mode 0700.
`crystal --init NAME` will create both (Phase 2); until then, create them
by hand for testing:

```sh
mkdir -m 700 -p ~/Vaults/diary && chmod 700 ~/Vaults
```

## Memory locking (required)

**Why.** Crystal locks all of its memory with `mlockall` so decrypted notes
never reach swap or a hibernation image (DESIGN.md, startup step 4). Locked
memory counts against `RLIMIT_MEMLOCK`. Fedora's default is 8 MiB, which a
large vault could exhaust mid-session; a failed allocation then aborts
without zeroizing memory or saving edits. So Crystal requires at least
**256 MiB** (`harden::MEMLOCK_FLOOR`) and refuses to start below that:

```text
crystal: RLIMIT_MEMLOCK is 8192 KiB, need at least 262144 KiB to keep notes out of swap; see docs/SETUP.md
```

**Check the current limit** (in KiB; `unlimited` is fine too):

```sh
ulimit -l
```

**Raise it** (needs root once). Replace `user` with your login name
(`id -un`). The value is in KiB: 262144 KiB = 256 MiB.

```sh
echo 'user - memlock 262144' | run0 tee /etc/security/limits.d/90-crystal.conf
```

`-` sets both the soft and hard limit. On Fedora and secureblue, `pam_limits`
runs in the `system-auth` PAM stack, which `systemd-user` includes, so the
limit applies to your systemd user manager and every terminal it starts.

**Apply it:** reboot, or log out of every session so the systemd user manager
restarts. Opening a new terminal is not enough.

**Verify:**

```sh
ulimit -l                                              # expect 262144
grep 'locked' /proc/$(pgrep -u "$(id -un)" -x systemd)/limits
```

The file itself may be readable by root only; that is fine, `pam_limits`
reads it as root.

**Trade-off.** Any process running as your user can now lock up to 256 MiB
of RAM. On a single-user desktop this is a minor denial-of-service risk; it
does not let anything read Crystal's memory.

**Undo:** `run0 rm /etc/security/limits.d/90-crystal.conf`, then reboot.

## ptrace and SELinux `deny_ptrace`

secureblue sets the SELinux boolean `deny_ptrace` to on. It blocks all
ptrace for normal users, including a parent tracing its own child. This is
stronger than Yama `ptrace_scope` and protects Crystal against same-user
malware (DESIGN.md threat A6). **Keep it on.**

Side effects for development:

- `strace` and `gdb -p` fail with `Permission denied` for every process, so
  "`gdb -p` fails" proves nothing on such a host. Crystal's own test
  (`harden::tests::lock_down_process_applies_every_setting`) reads the
  kernel's dumpable flag instead.
- Tests that need strace (Phase 4: only ciphertext is written) need the
  boolean off temporarily. Without `-P` the change does not survive a
  reboot:

  ```sh
  run0 setsebool deny_ptrace off
  # run the strace test
  run0 setsebool deny_ptrace on
  ```

Check its state with `getsebool deny_ptrace`.

## Development tools

| Tool | Version | Install | Used for |
| --- | --- | --- | --- |
| Rust toolchain | pinned in `rust-toolchain.toml` | rustup installs it on first `cargo` run | build, clippy, rustfmt, musl target |
| cargo-deny | 0.20.2 | `cargo install --locked cargo-deny --version 0.20.2` | `cargo deny check` |
| cargo-audit | 0.22.2 | `cargo install --locked cargo-audit --version 0.22.2` | `cargo audit` |
| strace | any | `brew install strace` | syscall and disk-leak tests (see above) |

CI installs the same cargo-deny and cargo-audit versions
(`.github/workflows/ci.yml`); bump both places together.
