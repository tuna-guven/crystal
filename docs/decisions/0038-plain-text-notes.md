# 0038 Plain text notes: titles ending in `.txt`

- **Date:** 2026-11-08 (Phase 3)
- **Status:** Accepted

## Context

Not every note is Markdown: lists of commands, logs, configuration
snippets and text pasted from elsewhere read better exactly as written,
without `#` turning into a heading or `*` into emphasis. Users should be
able to create and use plain text notes alongside Markdown ones.

Obsidian names Markdown notes without their `.md` and every other file
with its extension (`todo.txt`), and `[[todo.txt]]` links to it.

## Decision

- **A note whose title ends in `.txt` (any case) is plain text;** every
  other note is Markdown (`md::render::kind_of`). `.txt` alone is a
  Markdown note named `.txt`; a dot elsewhere (`v1.2 plan`) changes
  nothing.
- **No format change:** the title is the only marker. `mv todo.txt todo`
  makes it Markdown, and back.
- **Plain text is shown as written:** sanitized ([0037](0037-visible-escapes.md))
  and wrapped, nothing parsed. Links (`[[…]]`) and tags (`#tag`) are not
  read from plain text notes, as in Obsidian; other notes can still link
  to one by its name, `[[todo.txt]]`.
- **Export** (later) writes `title.md` for Markdown notes and the title
  as is for plain text, so an exported vault opens in Obsidian.

## Alternatives

- **A `kind` field in the note:** one more field to keep in step with the
  name, and an export would still need an extension to show it.
- **Any extension is plain text** (`.yaml`, `.log`): titles with a dot
  would change kind by accident; `.txt` is the one that is common and
  clear. More can be added later.
- **No plain text:** pasted text with `*`, `#` or `_` would render
  wrongly.

## Consequences

- `ls` shows plain text notes with their `.txt`, so the kind is always
  visible.
- The editor (Phase 4) is the same for both kinds; only the pager differs.
