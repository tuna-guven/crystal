# 0049 The shell starts at the top, like a normal shell

- **Date:** 2026-11-14 (Phase 5)
- **Status:** Accepted. Replaces the prompt's place in
  [0033](0033-shell.md) ("on the bottom row"); the rest of 0033 stays in
  force.

## Context

[0033](0033-shell.md) put the prompt on the bottom row with output
scrolling above it, so a fresh session shows one line at the bottom of an
empty screen. That reads as odd: a terminal shell starts at the top. The
place of the prompt has nothing to do with leaks: the shell draws on the
alternate screen, which never enters the terminal's scrollback and is
cleared when Crystal exits; that stays.

## Decision

- **The prompt follows the output,** as in a terminal: a new session
  shows the unlock message on the first rows and the prompt right below
  it; each command's output pushes the prompt down.
- **Once the screen is full,** output scrolls and the prompt stays on the
  bottom row, as before; PgUp and PgDn scroll the output as before.
- **`clear`** empties the output, so the prompt returns to the top row.

## Alternatives

- **Keep the bottom row:** no change, but the empty screen above the
  prompt is what looked wrong.

## Consequences

- A few dozen lines in `Shell::draw`: the output's rows at the screen's
  width decide where the prompt goes.
- Nothing else changes: the alternate screen, the scrollback limits and
  wiping are as in [0033](0033-shell.md).
