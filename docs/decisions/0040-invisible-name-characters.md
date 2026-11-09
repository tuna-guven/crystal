# 0040 Invisible characters are not allowed in note names

- **Date:** 2026-11-09 (Phase 3)
- **Status:** Accepted. Adds to the name rules of [0034](0034-vault-format-2.md).

## Context

The escape-injection test set renders a wikilink holding zero-width
characters, `[[ro​ad‍map]]`, as "road‍map": on screen the same as
"roadmap". In note text that is harmless. In a note's name it is a
spoofing risk: two notes named `roadmap` and `road​map` look the same
in `ls`, in completion, in the pager's title and in links, so a link can
lead to a note other than the one it appears to name.

## Decision

A title or folder segment may not hold an invisible character with no use
in a name: zero-width space (U+200B), word joiner (U+2060), byte-order mark
(U+FEFF), soft hyphen (U+00AD) and Mongolian vowel separator (U+180E).
They are refused like the reserved and control characters, when a vault is
read and when it is written (`model::name::check_segment`).

Zero-width joiner (U+200D) and zero-width non-joiner (U+200C) stay
allowed: emoji sequences (👨‍👩‍👧) and scripts such as Persian need them.

## Alternatives

- **Strip them silently:** two different stored names would become one.
- **Show them spelled out in names:** every list would carry `<U+200B>`
  markers; refusing them at the source is simpler.
- **Refuse all format characters (category Cf):** would refuse the
  joiners that emoji and some scripts need.

## Consequences

- A vault holding such a name, written by something other than Crystal,
  is refused, like any other broken name rule (fail closed).
- Confusable letters from different scripts (Latin `a`, Cyrillic `а`)
  remain possible; telling them apart needs a confusables table, which is
  more than Crystal carries.
