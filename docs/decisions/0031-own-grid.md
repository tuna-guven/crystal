# 0031 Own screen grid and terminal backend, no ratatui

- **Date:** 2026-11-08 (Phase 3)
- **Status:** Accepted; the U+FFFD replacement is superseded by
  [0037](0037-visible-escapes.md) (spelled-out escapes, `?` as the grid's
  fallback)

## Context

Crystal is meant to be a minimal, auditable, secure note-taking app with
as few dependencies as possible. The viewer needs a cell grid with Unicode
widths, a few widgets, and a CommonMark parser. The first plan was the
full `ratatui` with `crossterm` as its backend.

Crystal already owns the terminal: `term::TerminalGuard` sets raw mode
and the alternate screen on `/dev/tty`, restores them on every exit path,
and reads keys through `ppoll` with the signal self-pipes, with no threads
([0009](0009-signals-self-pipe.md)). `crossterm` would duplicate all of
that, bring its own event reader and signal handling, and add syscalls the
seccomp allowlist would have to admit.

A first step kept `ratatui-core` and `ratatui-widgets` with Crystal's own
backend. Writing that backend showed how little of ratatui Crystal uses:

- a grid of cells with display widths, and the difference between two
  grids;
- no constraint layout: the screen is fixed splits (finder, note pane,
  backlinks, status line);
- four simple widgets (a list, a scrolling text pane, a border, a status
  line), where only word wrapping is tricky.

For that, the two crates brought 28 crates into the build: build-time proc
macros (`darling`, `strum_macros`, `instability`, `indoc`), three crates
with `unsafe` code (`hashbrown`, `lru`, `compact_str`), and a Zlib licence
exception for `foldhash`. All of it would handle untrusted note text.
ratatui's defaults also needed working around: its diff debug-asserts on a
control character in a cell, and indexing outside a buffer panics.

The Phase 4 editor points the same way: `tui-textarea` needs the full
`ratatui` and `crossterm`, both ruled out, so the editor will be built on
whatever grid Crystal has.

## Decision

- **No `ratatui`, no `crossterm`;** `deny.toml` allows no Zlib.
- **Own backend (`ui::frame`):** Crystal turns the difference between two
  grids into cursor moves (CUP) and styles (SGR) and writes it to the
  `/dev/tty` fd it already holds. Every byte that reaches the terminal
  passes through Crystal's code.
- **Own grid (`ui::grid`):** one cell per column and row. A cell holds one
  grapheme inline (at most 16 bytes), its width and its style; a grid is
  one allocation that never grows and is wiped on reset and drop. Text
  enters only through `Grid::put_str`, which splits it into graphemes,
  measures each, and replaces with U+FFFD anything that is a control or
  BiDi character, wider than 2 columns, or longer than 16 bytes. A wide
  grapheme takes two cells; overwriting either half blanks the other.
  Writes outside the grid are clipped, never a panic.
- **Own colors and attributes:** the 16 named colors, the 256 indexed ones
  and the default; bold, dim, italic, underlined, reversed, crossed out.
  No blink, no hidden text, no RGB.
- **Two small crates instead** (exact pins, no dependencies of their own):
  - `unicode-width =0.2.3`, without default features (no `cjk` tables):
    the display width of a grapheme. `forbid(unsafe_code)`.
  - `unicode-segmentation =1.13.3`: grapheme clusters, so a character with
    its combining marks, or an emoji sequence, fills one cell.
    `deny(unsafe_code)`.

  Embedding Unicode width and segmentation tables in Crystal would be more
  code to keep current, not less risk.
- **`pulldown-cmark =0.13.6`** without default features: no `getopts`
  (its command-line tool) and no HTML writer; Crystal renders the events
  itself and sanitizes every text span. Without the `simd` feature it
  forbids `unsafe` code.
- `ui::frame` (backend) and `ui::screen` (two grids, full redraw on start,
  resize and resume) work on the grid. Widgets, wrapping and the editor
  will be Crystal's own.

## Alternatives

- **`ratatui` with `crossterm`:** a second terminal owner, with its own
  raw-mode, signal and event code, and more syscalls.
- **`ratatui-core` and `ratatui-widgets` with an own backend:** well
  tested, but most of their code and crates go unused, and every crate is
  something to audit.
- **pulldown-cmark's HTML writer:** a ready-made output that the
  sanitizer could be bypassed by.
- **No Unicode crates, own tables:** a few thousand lines of generated data
  to update with each Unicode release.
- **Treat every `char` as one column:** wrong for wide (CJK, emoji) and
  combining characters; the screen would drift out of line with the grid.

## Consequences

- 3 new crates for the viewer instead of 29: `pulldown-cmark` (its
  `memchr` and `unicase` were in the tree already) and the two Unicode
  crates. No build-time proc macro in the UI path, no `unsafe`
  added, no licence exception.
- Wrapping, widgets and the editor are Crystal code to write and test:
  property tests (no line wider than its area; every grapheme kept or
  shown as U+FFFD) and the Phase 3 renderer fuzzing cover them.
- Grid cells never allocate, so note text on screen is wiped with the grid,
  without waiting for the zeroizing allocator (Phase 5).
- Terminals can disagree with `unicode-width` on some emoji; a line may
  then look shifted until the next full redraw. ratatui has the same limit.
