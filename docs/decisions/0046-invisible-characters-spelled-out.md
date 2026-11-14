# 0046 Invisible characters inside a visible grapheme are spelled out

- **Date:** 2026-11-14 (Phase 4 review)
- **Status:** Accepted. Extends [0037](0037-visible-escapes.md), whose
  rules stay in force, to characters that hide behind a visible one.

## Context

[0037](0037-visible-escapes.md) spells out controls and BiDi characters.
Other characters draw nothing, or a blank, yet are not spaces: tag
characters, variation selectors, zero-width characters, the word joiner,
the byte order mark, the combining grapheme joiner and the Hangul
fillers. Most of them join the grapheme before them, so `a` and a tag
character draw exactly like `a`. Two notes or links can look the same
and differ, and a single emoji can carry any amount of hidden data in
variation selectors ("emoji smuggling"). The pager passed them through,
and the grid dropped the ones of no width without a trace.

## Decision

- **A grapheme that carries one is spelled out whole,** each character as
  dimmed `<U+XXXX>` (`ui::grid::hides_something`), in the editor
  ([0045](0045-simple-editor.md)) and in the sanitizer (`md::safe`), so
  the pager and the editor show the same thing. The set
  (`ui::grid::is_invisible`): U+034F, U+115F, U+1160, U+200B to U+200F,
  U+2060 to U+2064, U+3164, U+FE00 to U+FE0F, U+FEFF, U+FFA0, the tag
  characters U+E0000 to U+E007F, and U+E0100 to U+E01EF.
- **Emoji keep what they need:** a joiner (U+200D) between two
  pictographs, and U+FE0F in a grapheme that starts with a pictograph or
  is a keycap (`👨‍💻`, `❤️`, `1️⃣`). "Pictograph" is a short range
  table (`ui::grid::is_pictographic`), a little wider than Unicode's
  Extended_Pictographic, which only lets these two stay after an unusual
  symbol.

## Alternatives

- **Spell out only the invisible character, not its grapheme:** clearer
  to read, but splitting a grapheme needs a second drawing path in the
  editor and the sanitizer, for text that is suspicious anyway.
- **A Unicode property crate for Extended_Pictographic:** exact, but a
  dependency for a table of 17 ranges ([dependencies](../design/dependencies.md)).
- **Leave the pager as it was:** the rendered view would still hide what
  the editor shows.

## Consequences

- The three flags built from tag characters (England, Scotland, Wales)
  are spelled out.
- Japanese text with ideographic variation selectors (U+E0100 on) is
  spelled out; such sequences are rare in notes.
- `tests/corpus/escape/zero-width.golden` now shows the hidden U+200B and
  the joiner instead of `road‍map`.
