# 0054 Web and mail links are clickable (OSC 8), without their address

- **Date:** 2026-11-15 (after Phase 5)
- **Status:** Accepted.

## Context

The viewer showed `[text](url)` as underlined text with the address dim
in parentheses after it. The user asked for the text itself to be
clickable, as in HTML. Terminals offer this without Crystal capturing
the mouse ([0052](0052-viewer-cursor-and-folding.md)): OSC 8 hyperlinks
(`ESC ] 8 ; ; TARGET ESC \` before the text, `ESC ] 8 ; ; ESC \` after
it). The terminal opens the target, in Ptyxis on Ctrl+click; Crystal
spawns nothing and sends nothing over a network.

It is the first time note-chosen text goes inside an escape sequence
rather than into a cell, so the escape-injection defence
([0037](0037-visible-escapes.md)) needs a matching rule for it.

## Decision

- **What is clickable:** `[text](target)` and `<target>` links in the
  viewer whose target, as the Markdown parser decoded it, passes
  `md::link::clickable`:
  - the scheme is `http://`, `https://` (with a host) or `mailto:` (with
    an address), in any case;
  - every byte is printable ASCII that RFC 3986 allows in a URI
    (letters, digits, `-._~:/?#[]@!$&'()*+,;=%`), and every `%` starts a
    `%XX` escape; so ESC, BEL, the C1 terminator, spaces, controls and
    BiDi characters can never be in it;
  - at most 2083 bytes (VTE's limit).

  Anything else (`file:`, `javascript:`, app schemes, non-ASCII, an
  entity decoding to ESC) is drawn as before, not clickable, its address
  dim after it. Nothing is escaped, encoded or repaired. Images,
  `[[links]]`, the editor and the shell have no OSC 8.
- **The address of a clickable link is not shown**, at the user's
  request after the warning below; the text is underlined and cyan as
  before.
- **Three checks:** the renderer when it reads the link, the grid when a
  link is added (`Grid::add_link`), and the frame writer right before
  writing it. A cell holds a link number; the grid holds the frame's
  targets (at most 1024, zeroizing). A cell whose target changed counts
  as changed, so the terminal never keeps an old target under new text.
- **Always closed:** the frame writer opens a link where its cells start
  and closes it where they end and before the frame ends; the full
  redraw, the clear before a prompt and leaving the screen start with a
  close, so a frame broken off by a failed write cannot leave a link
  open into a prompt or the shell after Crystal.

## Alternatives

- **All schemes clickable** (the user's first choice): a click would
  hand the target to whatever handler the desktop has for it (`file:`
  opens local files; installed apps register schemes, some of which act
  on their own). Declined by the user at step L1.
- **Keep the address after clickable text** (recommended): the only
  way to see where a link goes before clicking. The user chose to hide
  it; the risk is below.
- **Show the address only when it differs from the text:** about 15
  lines; not chosen.
- **Let the terminal find plain URLs itself:** no code, but only the
  address is clickable, not the text, and a wrapped address breaks.

## Consequences

- About 140 lines (the check, the grid's links, the writer, the
  renderer), plus tests: unit and property tests on the check, the grid
  and the bytes written; a corpus file with hostile targets in every link
  form; a pty test through the binary; a manual click check
  (`scripts/check-links.sh`). No new syscall, dependency or process.
- **Accepted risk, deceptive links (threat A3):** with the address
  hidden, `[mybank.com](https://evil.example)` shows only "mybank.com",
  and a click opens evil.example. A note from an untrusted source (an
  import, a paste, a synced copy) can do this. Crystal still refuses
  every scheme but web and mail and every character that could act on
  the terminal; hidden characters in link text stay spelled out
  ([0046](0046-invisible-characters-spelled-out.md)). Ptyxis may show
  the target on hover. Read a link's target in the editor before
  clicking one from a note you did not write.
- A click is the terminal's: it may log or prefetch the target. The
  click opens a browser that will send a request to the target; Crystal
  itself never does.
