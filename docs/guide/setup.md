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
from `A-Z a-z 0-9 - _`, starting with a letter or digit. Every vault lives
in `~/.local/share/crystal/NAME`; there is no option to put it elsewhere
and no config file ([On-disk layout](../design/storage.md#on-disk-layout)).

```text
crystal NAME                 open the vault NAME (the Crystal shell)
crystal init NAME            create a vault
crystal recover NAME         set a new passphrase with the recovery code
crystal check-code NAME      check the recovery code on paper; changes nothing
crystal rm NAME              delete a vault for good
crystal help
```

`~/.local/share/crystal` and every vault in it must be yours and mode
0700; `~/.local` and `~/.local/share` must be yours and not writable by
group or others; none of them may be a symlink. Crystal checks this at
every start and never fixes a mode itself.

To sync a vault between devices (Syncthing, for example), sync its
directory `~/.local/share/crystal/NAME`. A vault that arrives on a new
device is opened with `crystal NAME`; the first unlock there pins it and
says so.

Edit a vault on one device at a time and let it sync before you open it
elsewhere. Crystal never merges two versions. If it finds that
`vault.age` changed on disk while you were editing, it refuses to save.
Your edits then exist only in the open editor and are lost when Crystal
closes; reopen the vault to see the other version. The check runs just
before writing, so a file that arrives in that very moment is still
overwritten: that is why one device at a time matters. If Syncthing made a conflict copy
(`vault.sync-conflict-….age`), Crystal refuses to open the vault until
you keep one version and move the other out of the vault directory.

### Creating a vault

```sh
crystal init diary
```

`crystal init` first checks that `~/.local/share/crystal/diary` does not
exist yet. Then it asks for:

1. **A new passphrase**, twice. It protects this vault's `identity.age`;
   each vault has its own (using the same one for several is your choice).
   Crystal recommends a passphrase of 8 or more random words, generated
   with KeePassDX or KeePassXC. It must have at least 20 characters
   ([decision 0028](../decisions/0028-passphrase-minimum.md)):
   `identity.age` travels with the vault, so anyone with a copy of it can
   guess offline.
2. **The recovery code.** Crystal generates a code of 7 groups of 4
   characters, such as `K7QM-3XRD-HW9P-T2NB-FG4S-ZE8V-YC6J`, and shows it
   once. Write it on paper, press Enter, and type it back to confirm; a
   mistake shows the code again. It opens `identity.backup.age`, so it
   recovers the vault if you forget the passphrase
   ([decision 0029](../decisions/0029-recovery-code.md)). Keep the paper
   like a house key: off the computer, and away from where the vault is
   synced or backed up, because the code and a copy of the vault together
   open your notes. Run `crystal check-code diary` once afterwards to make
   sure the paper is right.

Only then is anything created: `~/.local/share/crystal` (mode 0700) if
needed, the vault directory (0700) and its four files (0600). If a step
fails, what was created is removed again.

### If you forget the passphrase

```sh
crystal recover diary
```

`crystal recover NAME` asks for the recovery code. Case, dashes and spaces
do not matter, and O, I and L are read as 0, 1 and 1. A typo is reported
as such; another vault's code is refused. Then it asks for a new
passphrase twice and rewrites `identity.age`. Nothing is written until the
code, the vault and its rollback counter are checked. The recovery code
stays the same afterwards; keep the paper.

### Deleting a vault

```sh
crystal rm diary
```

Type the name again to confirm. Deleting is final: there is no trash, and
only a backup can bring the vault back. Files in the vault directory that
Crystal did not make are left alone, and so is the directory then.

### Opening a vault

`crystal NAME` asks for the passphrase once and unlocks the vault into the
Crystal shell ([decision 0033](../decisions/0033-shell.md)): a prompt at
the bottom of the screen, output above it. Nothing you see there stays in
the terminal's scrollback. The shell knows (`lock` comes with auto-lock in
Phase 5):

| Command | Does |
| --- | --- |
| `ls [FOLDER]` | Notes and folders as a tree (folders first, ignoring case) |
| `ls #TAG`, `tags` | The notes with a tag; every tag with its count |
| `find TEXT` | Names holding TEXT's letters in order, best match first |
| `search TEXT` | Notes whose name or text holds TEXT, ignoring case, with the matching lines (at most 100) |
| `view NAME` | The note, full screen. j/k and the arrows scroll, Space and PgDn/PgUp by a screen, g/G to the ends; Tab and Shift+Tab select `[[links]]`, Enter follows one (to a note, `#Heading` or `#^block`), Backspace goes back; b lists the notes linking here; q returns to the shell. A bare title works when only one note has it |
| `links` | Every `[[link]]` that leads nowhere, and why |
| `help`, `clear`, `quit` | `quit` also as `exit`, or Ctrl+D on an empty line |

Put a name with spaces in quotes: `view "meeting notes"`. Tab completes commands, note names and folders (a second Tab lists the
choices). On the prompt line, the arrow keys, Home and End move,
Ctrl+U, Ctrl+K and Ctrl+W delete, Up and Down recall earlier commands,
PgUp and PgDn scroll the output, Ctrl+C clears the line and Ctrl+L the
screen. Pasted text is always inserted as text, never run. The first
unlock of a vault on a device says so: rollback detection starts from that
point.

### Backups

Crystal has no backup code; use restic, borg or any file-level tool on
`~/.local/share/crystal` ([Backups](../design/backups.md)). Since deleting
is final, set this up together with Crystal. With restic and an external
drive:

```sh
restic init --repo /run/media/$USER/backup/crystal      # its own password, not Crystal's
restic --repo /run/media/$USER/backup/crystal backup ~/.local/share/crystal
restic --repo /run/media/$USER/backup/crystal forget --keep-daily 7 --keep-weekly 5 --keep-monthly 12 --prune
restic --repo /run/media/$USER/backup/crystal check
```

To restore, put the vault directory back with `restic restore` and open
it. If it is older than what this device last saw, Crystal says so and
asks you to type `restore`. Type it only if you restored the vault
yourself; otherwise someone may have put back an old copy.

### Copying

`y` in the pager copies the note into Crystal's own clipboard, which
never leaves Crystal; in the editor, Ctrl+C and Ctrl+X copy
the selection into it and Ctrl+V pastes from it. The terminal's own
Ctrl+Shift+C and Ctrl+Shift+V work with the system clipboard
([decision 0045](../decisions/0045-simple-editor.md)). Crystal
never writes to the system clipboard
([decision 0041](../decisions/0041-internal-clipboard-only.md)).

To copy into another program, select the text with the mouse and use the
terminal's copy key (Ctrl+Shift+C in Ptyxis). That copies what is shown,
with hidden characters spelled out as `<U+XXXX>`. What you copy this way
is on the system clipboard, where every program you run can read it and a
clipboard manager may keep it; Crystal cannot clear it.

## Memory locking (required)

**Why.** Crystal locks all of its memory with `mlockall` so decrypted notes
never reach swap or a hibernation image ([startup](../design/startup.md) step 4). Locked
memory counts against `RLIMIT_MEMLOCK`. Fedora's default is 8 MiB, which a
large vault could exhaust mid-session; a failed allocation then aborts
without zeroizing memory or saving edits. So Crystal requires at least
**512 MiB** (`harden::MEMLOCK_FLOOR`) and refuses to start below that:

```text
crystal: RLIMIT_MEMLOCK is 8192 KiB, need at least 524288 KiB to keep notes out of swap; see docs/guide/setup.md
```

**Check the current limit** (in KiB; `unlimited` is fine too):

```sh
ulimit -l
```

**Raise it** (needs root once). Replace `user` with your login name
(`id -un`). The value is in KiB: 524288 KiB = 512 MiB.

```sh
echo 'user - memlock 524288' | run0 tee /etc/security/limits.d/90-crystal.conf
```

`-` sets both the soft and hard limit. On Fedora and secureblue, `pam_limits`
runs in the `system-auth` PAM stack, which `systemd-user` includes, so the
limit applies to your systemd user manager and every terminal it starts.

**Apply it:** reboot, or log out of every session so the systemd user manager
restarts. Opening a new terminal is not enough.

**Verify:**

```sh
ulimit -l                                              # expect 524288
grep 'locked' /proc/$(pgrep -u "$(id -un)" -x systemd)/limits
```

The file itself may be readable by root only; that is fine, `pam_limits`
reads it as root.

**Trade-off.** Any process running as your user can now lock up to 512 MiB
of RAM. On a single-user desktop this is a minor denial-of-service risk; it
does not let anything read Crystal's memory.

**Undo:** `run0 rm /etc/security/limits.d/90-crystal.conf`, then reboot.

**Passphrase files.** The passphrase is stretched with scrypt, which needs
a lot of memory, and that memory is locked too. Crystal writes every
passphrase file at the same factor on every device, log N 18 (256 MiB),
which is half the 512 MiB floor, so any device that starts Crystal opens
any vault ([decision 0027](../decisions/0027-scrypt-fixed.md)). A larger
limit, up to what your machine can spare (for example 16777216 KiB =
16 GiB), is fine and changes nothing else. A passphrase file that needs
more than your limit allows is refused with a message pointing here,
never a crash.

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
- The strace test (Phase 4: only ciphertext is written) runs in CI only:
  Crystal is non-dumpable, so strace needs `CAP_SYS_PTRACE` to read what
  it writes, which CI gives it. There is nothing to run locally.

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
