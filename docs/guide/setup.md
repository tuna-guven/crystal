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
restorecon -Fv ~/.local/bin/crystal   # SELinux: must show crystal_exec_t
```

Check the label with `ls -Z ~/.local/bin/crystal`: it must read
`crystal_exec_t`, or Crystal runs outside its SELinux domain. `-F` is
needed: the default label of `~/.local/bin`, `home_bin_t`, is a
*customizable type*, which a plain `restorecon` silently leaves alone.

**The SELinux module** (once, and after each change to
`packaging/selinux/`; [Sandboxing, section 7](../design/sandboxing.md)).
Build it as yourself in a scratch directory, install it as root, then
relabel:

```sh
mkdir -p /tmp/crystal-selinux && cp packaging/selinux/crystal.{te,fc,if} /tmp/crystal-selinux/
make -C /tmp/crystal-selinux -f /usr/share/selinux/devel/Makefile crystal.pp
run0 semodule -i /tmp/crystal-selinux/crystal.pp
restorecon -RFv ~/.local/bin/crystal ~/.local/share/crystal ~/.local/state/crystal
```

While a vault is open, `ps -eZ | grep crystal` shows `crystal_t`. The
policy creates `~/.local/share/crystal` and `~/.local/state/crystal`
itself, but not missing parents: `~/.local/share` and `~/.local/state`
must exist (a normal desktop session has both).

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
the command: `crystal unlock diary 5>&-`. List what is open with
`ls -l /proc/$$/fd` in the same shell.

## Vaults

Vaults are opened by name: `crystal unlock diary`. Names are 1 to 64 characters
from `A-Z a-z 0-9 - _`, starting with a letter or digit. Every vault lives
in `~/.local/share/crystal/NAME`; there is no option to put it elsewhere
and no config file ([On-disk layout](../design/storage.md#on-disk-layout)).

```text
crystal unlock NAME          open the vault NAME (the Crystal shell)
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
device is opened with `crystal unlock NAME`; the first unlock there pins it and
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
   ([decision 0028](../decisions.md#0028)):
   `identity.age` travels with the vault, so anyone with a copy of it can
   guess offline.
2. **The recovery code.** Crystal generates a code of 7 groups of 4
   characters, such as `K7QM-3XRD-HW9P-T2NB-FG4S-ZE8V-YC6J`, and shows it
   once. Write it on paper, press Enter, and type it back to confirm; a
   mistake shows the code again. It opens `identity.backup.age`, so it
   recovers the vault if you forget the passphrase
   ([decision 0029](../decisions.md#0029)). Keep the paper
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

`crystal unlock NAME` asks for the passphrase once, at the top left
of the window under a cow that welcomes you ([decision 0058](../decisions.md#0058)),
and unlocks the vault into the Crystal shell ([decision 0033](../decisions.md#0033)): like a
terminal, it starts at the top, and the prompt follows the output down
the screen ([decision 0049](../decisions.md#0049));
`clear` or Ctrl+L brings it back to the top. Nothing you see there stays in
the terminal's scrollback. The shell knows:

| Command | Does |
| --- | --- |
| `ls [FOLDER]` | Notes and folders as a tree (folders first, ignoring case). Each note shows its type dimmed after its name: `Linux Kernel · markdown`, `todo · text`; lists show it the same way; title bars show only the name ([decision 0038](../decisions.md#0038)) |
| `ls #TAG`, `tags` | The notes with a tag; every tag with its count |
| `find TEXT` | Names holding TEXT's letters in order, best match first |
| `search TEXT` | Notes whose name or text holds TEXT, ignoring case, with the matching lines (at most 100) |
| `view NAME` | The note, full screen, headings marked `◉ ○ ● ✿ ◆ ◇` by level with their sections indented under them. A line break in the note stays one. Tables are boxed; in a narrow window their cells wrap inside the box. A bold orange `>` on the left marks the cursor's line; the viewer shows no line numbers, but j/k, the arrows and the mouse wheel move the cursor from one line of the note to the next as the editor numbers them (lines with text only: not blank lines, code fences or a table's `|---|` line), stopping at the last one, Space and PgDn/PgUp by a screen, g/G to the ends. Tab on a heading folds its section (` …`) or unfolds it; folds are not saved. Web and mail links (`[text](https://…)`, `<https://…>`, `mailto:`) are clickable: Ctrl+click in Ptyxis opens them, and their address is not shown; other links show their address and are not clickable. A link's text can name another site than the one it opens: in a note you did not write, read the target in the editor before clicking ([decision 0054](../decisions.md#0054)). n and N select the next and previous `[[link]]`, Enter follows it (to a note, `#Heading` or `#^block`), Backspace goes back; b lists the notes linking here; y copies the cursor's line and Y the whole note into Crystal's clipboard; q returns to the shell. A bare title works when only one note has it ([decision 0052](../decisions.md#0052), [decision 0053](../decisions.md#0053), [decision 0055](../decisions.md#0055), [decision 0057](../decisions.md#0057)) |
| `edit NAME` | The note in the editor: Ctrl+S saves, Ctrl+E shows it rendered, Ctrl+Q closes. When no note has that name (`FOLDER/NAME` for one in a folder), Crystal asks "Create it?" (type y), then its type: m or Enter for Markdown, t for plain text. The type cannot change later, and a name is typed without `.txt`. The note exists once Ctrl+S saves it |
| `mv OLD NEW` | Renames or moves a note, keeping its type; `[[links]]` to it follow |
| `rm NAME` | Deletes a note for good, after you type y |
| `links` | Every `[[link]]` that leads nowhere, and why |
| `lock` | Wipes the notes, the shell's output and history and Crystal's clipboard from memory, and shows the lock screen (a cow and the passphrase prompt, at the top left). A wrong passphrase there asks again; Esc or Ctrl+C quits. Crystal also locks by itself after 5 minutes without a key, in the shell, the viewer and the editor; unsaved edits are saved first ([decision 0048](../decisions.md#0048)) |
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
yourself; otherwise someone may have put back an old copy. With no key
for 5 minutes at that question, Crystal quits and accepts nothing; the
next unlock asks again.

### Copying

`y` in the pager copies the line the `>` is on, as written, and `Y` the
whole note, into Crystal's own clipboard, which never leaves Crystal
([decision 0053](../decisions.md#0053)); in
the editor, Ctrl+C and Ctrl+X copy
the selection into it and Ctrl+V pastes from it. The terminal's own
Ctrl+Shift+C and Ctrl+Shift+V work with the system clipboard
([decision 0045](../decisions.md#0045)). Crystal
never writes to the system clipboard
([decision 0041](../decisions.md#0041)).

Text pasted with Ctrl+Shift+V is untrusted and arrives as plain text:
control characters and BiDi overrides are removed and broken bytes
replaced, and invisible characters are shown as `<U+XXXX>`. When anything
was removed or replaced, the editor and the shell say "Untrusted input sanitized and
pasted safely."; Ctrl+Z undoes the paste. Formatting (HTML) is never
kept; convert it to Markdown before copying if you need it
([decision 0047](../decisions.md#0047)).

To copy into another program, select the text with the mouse and use the
terminal's copy key (Ctrl+Shift+C in Ptyxis). That copies what is shown,
with hidden characters spelled out as `<U+XXXX>`, and the editor's line
numbers (or the pager's `>`) in front of each row too; hold Ctrl while
dragging for a block selection that leaves them out, where the terminal
has one. Unfold a section before selecting it: a fold's hidden lines are
not on screen, so they are not copied. What you copy this way
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
any vault ([decision 0027](../decisions.md#0027)). A larger
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
  it writes and `CAP_DAC_READ_SEARCH` to name the files it writes to,
  which CI gives it. There is nothing to run locally.

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
