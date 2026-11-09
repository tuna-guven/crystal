# 0033 The Crystal shell

- **Date:** 2026-11-08 (Phase 3 redesign)
- **Status:** Accepted. Replaces the planned multi-pane viewer (fuzzy
  finder and backlinks panes) of the roadmap's Phase 3. The `copy` command
  and system-clipboard copying are replaced by
  [0041](0041-internal-clipboard-only.md).

## Context

The first plan for Phase 3 was a multi-pane TUI: a fuzzy finder, a note
pane and a backlinks pane. A command shell, like gopass's commands, is
easier to learn, easier to extend one command at a time, and simpler to
build: one prompt, one scrolling output area, and two full-screen views.

Note text on the terminal's normal screen stays in its scrollback (and in
tmux history) after Crystal exits, and anything printed to stdout can be
piped or logged.

## Decision

- **`crystal NAME` unlocks and opens the shell** on the alternate screen:
  a prompt `crystal:NAME>` on the bottom row, command output scrolling
  above it. There are no one-shot commands that print note content
  (`crystal NAME show x` does not exist).
- **Memory only:** the output scrollback (at most 10 000 entries) and the
  command history (at most 200 entries) are held in zeroizing memory and
  wiped on exit, and on `lock` and suspend once those lock (Phase 5). Nothing reaches the terminal's
  scrollback, a file or stdout.
- **Commands** (Phase 3, read-only):

  | Command | Does |
  | --- | --- |
  | `ls [FOLDER]` | Notes and subfolders as a tree ([0034](0034-vault-format-2.md)) |
  | `ls #TAG`, `tags` | Notes with a tag; all tags with counts |
  | `view NAME` | Full-screen rendered pager: scroll, follow links, backlinks, copy ([0041](0041-internal-clipboard-only.md)) |
  | `search TEXT` | Full text over names and bodies; one matching line per hit |
  | `find TEXT` | Fuzzy match on names |
  | `links` | Broken links: missing notes, headings or block ids |
  | `copy NAME` | Replaced: no system clipboard ([0041](0041-internal-clipboard-only.md)) |
  | `help`, `clear`, `lock`, `quit` | |

  Phase 4 adds `edit NAME` (full-screen editor), `new NAME`, `mv OLD NEW`
  (rewriting links to the note) and `rm NAME` (asks for `y`; final, no
  trash, as for vaults in [0032](0032-fixed-locations.md)).
- **Parsing:** words split on spaces, double quotes group a name with
  spaces, backslash escapes a quote. No pipes, redirection, globbing,
  variables or command substitution, and nothing is ever run outside
  Crystal.
- **Input:** a one-line editor in a fixed-size zeroizing buffer (as for
  the passphrase prompt, [0016](0016-passphrase-prompt.md)), echoed
  through the grid like all output. Tab completes commands and note
  names; Up and Down walk the history.
- **`lock`** wipes the notes, scrollback, history and internal clipboard
  and returns to the passphrase prompt; `quit` exits.
- Everything is drawn on Crystal's own grid
  ([0031](0031-own-grid.md)): the shell, the pager and the editor.

## Alternatives

- **Multi-pane TUI (the first plan):** more layout and focus handling for
  the same features; the shell's `find` and the pager's backlinks cover
  the panes.
- **A line shell on the normal screen:** simpler drawing, but note names
  and text would stay in the terminal's scrollback.
- **One-shot commands printing to stdout, like gopass:** convenient for
  scripts, but every use puts plaintext into pipes, logs and scrollback.

## Consequences

- Full-text search moves from Phase 5 to Phase 3, because `search` needs
  it.
- The shell's scrollback is a second place note text sits in memory; it
  is bounded, zeroizing and wiped on lock.
- A command that grows the shell (a new verb) is a small, testable unit.
