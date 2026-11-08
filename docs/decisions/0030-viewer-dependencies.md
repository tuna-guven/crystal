# 0030 Viewer dependencies: ratatui-core and ratatui-widgets, own backend

- **Date:** 2026-11-08 (Phase 3)
- **Status:** Accepted

## Context

Phase 3 adds the read-only viewer: a fuzzy finder over titles, a Markdown
renderer and sanitizer, wikilinks and backlinks. It needs a cell buffer
with Unicode widths, layout, a few widgets, and a CommonMark parser. The
design planned the full `ratatui` crate with `crossterm` as its backend.

Crystal already owns the terminal: `term::TerminalGuard` sets raw mode and
the alternate screen on `/dev/tty`, restores them on every exit path, and
reads keys through `ppoll` with the signal self-pipes, with no threads
([0009](0009-signals-self-pipe.md)). `crossterm` would duplicate all of
that, bring its own event reader and signal handling, and add syscalls
that the seccomp allowlist would have to admit.

## Decision

- **`ratatui-core =0.1.2`** with only `std`: the `Buffer`, layout, text
  and style types. No terminal backend crate.
- **`ratatui-widgets =0.3.2`** without default features: `Paragraph`,
  `List`, `Block`, `Scrollbar`. This leaves out the calendar widget's
  `time` dependency and serde.
- **`pulldown-cmark =0.13.6`** without default features: no `getopts`
  (its command-line tool) and no HTML writer. Crystal renders the parser's
  events itself and sanitizes every text span.
- **Own backend:** Crystal turns the buffer into terminal output itself and
  writes it to the `/dev/tty` fd it already holds. Every byte that reaches
  the terminal passes through Crystal's code, after the sanitizer.
- **License:** `ratatui-core`, `ratatui-widgets`, `kasuari` and `lru` use
  `hashbrown` with its default hasher, which pulls in `foldhash 0.2.0`
  under the Zlib license. That cannot be switched off from Crystal's side.
  Zlib is permissive and GPL-3.0 compatible, so `deny.toml` allows it, with
  a comment naming the crate. foldhash hashes layout-cache keys only, never
  secrets.
- Exact pins: the renderer handles untrusted note text, so these crates
  change only by review.

## Alternatives

- **`ratatui` with `crossterm`:** the planned setup. It would be a second
  terminal owner, with its own raw-mode, signal and event code, and more
  syscalls for the seccomp allowlist.
- **No UI crate, an own cell buffer and layout:** no new dependency, but
  Unicode width, clipping, wrapping and diffing are exactly the code that
  is easy to get wrong; ratatui's is widely used and tested.
- **pulldown-cmark with the HTML writer:** not needed; a ready-made output
  is what the sanitizer must never be bypassed by.

## Consequences

- 29 new crates, 7 of them proc macros or their helpers that run only at
  build time (`darling`, `instability`, `indoc`, `strum_macros`). Among
  the rest, `hashbrown`, `lru` and `compact_str` contain `unsafe` code.
  They are widely used, and the renderer fuzzing of Phase 3 runs through
  them.
- The backend is Crystal code to write and test: cursor moves, styles and
  a diff against the last frame.
- `tui-textarea` (Phase 4) depends on the full `ratatui` and a backend
  crate. Phase 4 must check whether a version works on `ratatui-core`
  alone, or replace it with an own editor widget; that is decided then.
