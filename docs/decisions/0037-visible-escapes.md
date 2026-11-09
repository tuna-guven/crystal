# 0037 Hidden characters are spelled out, not replaced by U+FFFD

- **Date:** 2026-11-08 (Phase 3)
- **Status:** Accepted. Supersedes the U+FFFD replacement in
  [0031](0031-own-grid.md) (and in the clipboard record 0035, since
  replaced by [0041](0041-internal-clipboard-only.md)).

## Context

Note text can carry characters that act on the terminal (ESC, CSI and the
other C0 and C1 controls) or that hide and reorder text (the BiDi
controls of Trojan Source). The grid replaced each with U+FFFD (`�`).
U+FFFD comes from the terminal's font, and some terminal fonts do not have
it: it then shows as nothing, or as a blank box, which is exactly what a
replacement must not do. It also says only that *something* was there.

## Decision

- **The sanitizer (`md::safe::sanitize`) spells each one out** as
  `<U+XXXX>` (4 to 6 uppercase hex digits), drawn dimmed: plain ASCII, so
  visible in every font, and it shows what was hidden. A tab becomes four
  spaces; `\r\n` becomes `\n`; a lone `\r` is spelled out like any other
  control character.
- **`SafeText`** is the only type `sanitize` produces and the only way
  note text reaches a widget (from L5). It is note text: zeroizing, sized
  once, and without Debug, Display, Clone or Serialize.
- **The grid's last line of defence draws `?`** (ASCII) for anything that
  still arrives unsafe or unmeasurable: a forbidden character the
  sanitizer missed, a grapheme wider than two columns, or one longer than
  16 bytes (stacked combining marks). The frame writer's check uses the
  same `?`.

## Alternatives

- **Keep U+FFFD:** invisible in some fonts.
- **Drop hidden characters silently:** a Trojan Source note would look
  harmless; the point is to show that something is there.
- **`^[`-style caret notation:** covers only C0 controls, not C1 or BiDi
  characters, and is easy to mistake for text.

## Consequences

- A note with an embedded escape sequence shows it as text, for example
  `<U+001B>[2J`, dimmed.
- An escaped character is wider than the original (up to 10 columns);
  wrapping counts the escape's real width.
- A `?` on screen is either a real question mark or the grid's fallback;
  the sanitizer makes the fallback rare (zalgo text, or a bug).
