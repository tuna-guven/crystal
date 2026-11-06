# Setup

System settings Crystal needs, and how to develop on a hardened host.
Crystal checks these at startup and refuses to run if they are missing.

Root commands below use `run0` (systemd, authorized through polkit) instead
of `sudo`: secureblue ships no SUID `sudo`, which removes a whole class of
SUID privilege-escalation bugs. On other distributions `sudo` works the same.

## Installing

Crystal is a per-user executable in `~/.local/bin`; installing and updating
need no root ([Installation](installation.md)).

```sh
cargo build --locked --release
install -m 0555 target/x86_64-unknown-linux-musl/release/crystal ~/.local/bin/crystal
restorecon -v ~/.local/bin/crystal    # once the SELinux module exists; run0 if relabeling is denied
```

**Trade-off.** Any program running as you can write to `~/.local/bin`, so
it could replace Crystal with a fake that asks for your passphrase. Every
protection inside Crystal would then protect the fake.

**Optional: make it immutable.** Setting or clearing the immutable flag needs
root, so nothing running as you can overwrite, delete or rename the file:

```sh
run0 chattr +i ~/.local/bin/crystal
lsattr ~/.local/bin/crystal          # shows ----i---------
```

To update: `run0 chattr -i ~/.local/bin/crystal`, then the `install` and
`restorecon` lines above, then `run0 chattr +i ~/.local/bin/crystal`.
Relabel before setting the flag: an immutable file's SELinux label cannot
be changed either.

The flag protects the file, not the directory: `~/.local/bin` itself can
still be renamed and recreated with a fake inside. To close that too, run
`run0 chattr +i ~/.local/bin`; other per-user installs into it then need
the same `chattr -i` / `chattr +i` dance.

**Add an absolute-path alias** to `~/.bashrc`:

```bash
alias crystal="$HOME/.local/bin/crystal"
```

Why: `~/.cargo/bin` or any other directory earlier in `PATH` could hold a
planted `crystal`. Bash checks aliases before `PATH`. This only protects you
if `~/.bashrc` itself cannot be changed without root (mark it immutable too;
check with `lsattr ~/.bashrc`).

## Inherited file descriptors

Crystal refuses to start when the program that launched it left a file
descriptor above 2 open:

```text
crystal: 1 open file descriptor(s) inherited from the program that started Crystal, lowest 5; ...
```

An inherited descriptor would stay usable inside the sandbox (Landlock only
checks files opened after it is applied), and Crystal cannot close a
descriptor it does not own without `unsafe` code. Plain terminal shells do
not leak descriptors; IDE terminals, some launchers and wrapper scripts
sometimes do. Start Crystal from a plain shell, or close the descriptor in
the command: `crystal diary 5>&-`. List what is open with
`ls -l /proc/$$/fd` in the same shell.

## Vaults

Vaults are opened by name: `crystal diary`. Names are 1 to 64 characters
from `A-Z a-z 0-9 - _`, starting with a letter or digit. The encrypted
config file `~/.config/crystal/config.age` maps each name to its vault
directory ([On-disk layout](../design/storage.md#on-disk-layout)).

```text
crystal [--lock-after MINUTES] NAME   open the vault NAME
crystal init NAME [--at DIR]          create a vault in ~/Vaults/NAME or in DIR
crystal add NAME DIR                  register an existing vault as NAME
crystal remove NAME                   forget NAME; its files are kept
```

`~/.config/crystal` must be yours and mode 0700, and `~/.config` must be
yours and not writable by group or others; neither may be a symlink.
Crystal checks this at every start and never fixes a mode itself. Only
`crystal init` and `crystal add` create the directory; a normal run refuses to start
without it:

```text
crystal: ~/.config/crystal does not exist; see docs/guide/setup.md
```

### Creating a vault

```sh
crystal init diary                  # ~/Vaults/diary
crystal init work --at ~/Sync/work  # the directory itself; must not exist
```

`crystal init` first checks where the vault will go: the directory must not
exist, and its parent must be owned by root or you, not writable by group
or others, and reached without symlinks. Then it asks for:

1. **The passphrase.** On a device without a config, a new one, twice; it
   protects both the config and the vault's `identity.age`. On a device
   with a config, the config's passphrase.
2. **One or more backup recipients**: `age1…` public keys of keys you keep
   offline, such as a paper key made with `age-keygen` or a second
   hardware key. They go into `identity.backup.age`, which recovers the
   vault if you lose the passphrase. An empty line ends the list. Keep the
   matching secret key off this machine.

Only then is anything created: `~/Vaults` (mode 0700) if needed, the vault
directory (0700) and its four files (0600). If a step fails, what was
created is removed again.

### Registering and forgetting vaults

`crystal add NAME DIR` registers a vault that already exists, for example one
made by `crystal init` on another device and synced here. It asks for the
config's passphrase, or for a new one if this device has no config yet;
the vault's `identity.age` must open with the same passphrase. `crystal
remove NAME` forgets a vault and leaves its files alone.

### Opening a vault

`crystal NAME` asks for the passphrase once and unlocks the vault. Until
the viewer exists (Phase 3), it only shows how many notes the vault holds;
any key locks it again and quits. The first unlock of a vault on a device
says so: rollback detection starts from that point.

## Memory locking (required)

**Why.** Crystal locks all of its memory with `mlockall` so decrypted notes
never reach swap or a hibernation image ([startup](../design/startup.md) step 4). Locked
memory counts against `RLIMIT_MEMLOCK`. Fedora's default is 8 MiB, which a
large vault could exhaust mid-session; a failed allocation then aborts
without zeroizing memory or saving edits. So Crystal requires at least
**256 MiB** (`harden::MEMLOCK_FLOOR`) and refuses to start below that:

```text
crystal: RLIMIT_MEMLOCK is 8192 KiB, need at least 262144 KiB to keep notes out of swap; see docs/guide/setup.md
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

**More memory, stronger passphrase files.** The passphrase is stretched
with scrypt, which needs a lot of memory, and that memory is locked too.
Crystal lets scrypt use up to half of your limit
([decision 0025](../decisions/0025-scrypt-budget.md)). age's own default
takes about one second, which on a fast machine needs 256 MiB, so at the
256 MiB floor Crystal uses a smaller factor (128 MiB). For age's full
default, set the limit to at least 1 GiB (1048576 KiB); a larger limit,
up to what your machine can spare (for example 16777216 KiB = 16 GiB), is
fine and changes nothing else. A passphrase file that needs more than your
limit allows is refused with a message pointing here, never a crash.

## ptrace and SELinux `deny_ptrace`

secureblue sets the SELinux boolean `deny_ptrace` to on. It blocks all
ptrace for normal users, including a parent tracing its own child. This is
stronger than Yama `ptrace_scope` and protects Crystal against same-user
malware ([threat model](../design/threat-model.md), A6). **Keep it on.**

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
