# 0048 Lock and auto-lock: 5 minutes, save first, unlock again in place

- **Date:** 2026-11-14 (Phase 5)
- **Status:** Accepted. Settles what [0033](0033-shell.md) left for
  Phase 5 (`lock`, and suspend locking) and the lock key
  [0045](0045-simple-editor.md) left open.

## Context

An unlocked Crystal holds every note decrypted, the vault's recipient,
the shell's scrollback and history, Crystal's clipboard and any open
editor. A session left open on an unattended desk shows all of it to
whoever sits down, and keeps it in memory for whatever reads memory
later. [0033](0033-shell.md) planned a `lock` command and the threat
model an auto-lock (A8: "Idle auto-lock reduces exposure"), with the time
first meant to come from a settings file.

## Decision

- **Auto-lock after 5 minutes without input,** fixed. Only keys and
  pastes count as input (a resize does not). It runs in the shell, the
  pager and the editor alike: the input wait (`TerminalGuard::wait`)
  gets the time left as its timeout, measured on the monotonic clock.
  There is no setting and no way to turn it off; changing the time is a
  one-line change in the source.
- **`lock` in the shell** locks at once. There is no lock key in the
  editor or the pager: leave them (Ctrl+Q, q) and type `lock`, or let
  auto-lock do it.
- **Unsaved edits are saved first.** If the editor holds unsaved changes
  (a new note too), lock saves them as Ctrl+S would, then locks. When the
  save is refused (`vault.age` changed on disk since unlock,
  [0045](0045-simple-editor.md)) or fails, Crystal locks anyway: the
  edits are lost, and the lock screen says so. Locking never waits on
  the user.
- **Lock wipes everything that came from the vault:** the notes and their
  indexes, the vault's recipient, the shell's scrollback and history,
  Crystal's clipboard, the pager's page and the editor's text and undo
  log. It keeps only what is not secret: the vault's name and its
  directory and state directory handles, so it can unlock again.
- **The lock screen** is the passphrase prompt, with a fixed cow above it
  (cowsay's default cow, GPL like Crystal):

  ```
  / I locked your vault to protect you \
  \ while you were away.               /
   ------------------------------------
          \   (__)
           \  (oo)\_______
              (__)\       )\/\
                  ||----w |
                  ||     ||
  ```

  followed by one line on what happened (locked by `lock` or after 5
  minutes; edits saved, or lost because the save was refused) and
  `Passphrase:`.
- **Unlock again in the same process,** through the same steps as at
  start: open `identity.age` with the passphrase, decrypt `vault.age`,
  check the pin and the generation counter; an older vault asks for
  `restore` ([0036](0036-backups-external.md)). A wrong passphrase asks
  again. Esc, Ctrl+C or an exit signal at the lock screen quits Crystal,
  with nothing left to wipe.
- **Suspend locks first:** a `SIGTSTP` sent from outside locks (saving
  as above), then suspends, so a stopped Crystal holds no notes; after
  `SIGCONT` the lock screen says it was suspended. At the passphrase
  prompts and questions a `SIGTSTP` is ignored and dropped: there is
  nothing to lock, and at the `restore` question the just-decrypted
  vault would stay in the stopped process.
- **No settings:** Phase 5 adds no settings, and the vault format stays
  at 2. Where settings go is decided when the first one is needed
  (a wish on record: inside `vault.age`, so they are encrypted and
  synced). Colors follow the terminal's theme: Crystal uses only the 16
  standard colors, so a theme set in the terminal applies to Crystal.

## Alternatives

- **Lock by exiting:** simpler (no second unlock path), but every idle
  timeout would end the session, and the next start is the same unlock
  anyway.
- **Discard unsaved edits on lock:** keeps lock free of writes, but loses
  work after 5 minutes away.
- **A settings file or a `set` command for the time:** a parser and a
  file, or vault format 3, for one number; turning auto-lock off would
  remove a protection.
- **In-vault themes (`set theme`):** about 300 to 450 lines (a theme
  table, 24-bit color, the settings above) for what the terminal's theme
  already does.

## Consequences

- Unlocking again inside the sandbox (Phase 6) reads only the vault and
  state directories, already allowed; its prompt's `tcflush` needs
  `TCFLSH` in the ioctl allowlist ([Syscalls](../reference/syscalls.md)).
- Without the zeroizing allocator, freed copies could outlive a lock;
  the allocator comes first in Phase 5, and the canary scan
  ([Testing](../development/testing.md)) checks the result.
- A lock in the middle of a `rm` question or any prompt simply ends it;
  nothing is deleted or changed by a lock but the auto-save.
