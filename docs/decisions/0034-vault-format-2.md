# 0034 Vault format 2: folders as metadata, links and tags from the text

- **Date:** 2026-11-08 (Phase 3 redesign)
- **Status:** Accepted. Supersedes the note layout and `VERSION` rule of
  [0018](0018-vault-format-1.md); the single encrypted blob of
  [0001](0001-single-vault-blob.md) stays.

## Context

The shell ([0033](0033-shell.md)) addresses notes by name and shows them
as a tree, like gopass. Notes stay inside the one padded `vault.age`,
which hides how many there are, their sizes and which one changed;
folders exist only as metadata. Links must stay Obsidian-compatible,
including links to a section or a single block of a note. A separate
metadata file, or a stored link table, would be a second source of truth
that a crash, a partial sync or a rollback could put out of step with the
notes.

## Decision

- **One blob, postcard, records:** the decrypted vault is a small
  database of note records, encoded with postcard (already a dependency;
  no CSV, which would need quoting rules and a parser for multi-line
  Markdown):

  ```text
  format: u16 = 2 | vault_id: [u8; 32] | generation: u64 | notes: [Note]
  Note = id: [u8; 16] | folder: str | title: str | created: u64 | modified: u64 | body: str
  ```

- **Folders are metadata:** `folder` is `""` (top level) or segments
  joined by `/`, such as `work/projects`, at most 32 levels and 1 KiB. A
  note's **name** is `folder/title`, or `title` at the top level. Folders
  exist only as prefixes of names: there are no empty folders, and `ls`
  builds the tree from the names.
- **Name rules (Obsidian-compatible, also valid file names for an
  export):** a title or folder segment is 1 to 255 bytes, has no leading
  or trailing space, is not `.` or `..`, and contains none of
  `[ ] # ^ | \ / : * ? " < >`, nor a control or BiDi character. Names are
  unique in a vault, compared case-insensitively (as Obsidian resolves
  links). Decode and encode both enforce the rules, so Crystal never
  writes a vault it would refuse to read.
- **Links live in the text, Obsidian syntax:** `[[name]]`,
  `[[name|shown text]]`, `[[name#Heading]]`, `[[name#^block-id]]`,
  `[[#Heading]]` within the same note. A target without `/` matches a
  title when exactly one note has it; otherwise the full name is needed.
  A block id `^id` (letters, digits, `-`) stands at the end of a
  paragraph or list item, as in Obsidian. `![[…]]` embeds are shown as
  plain links.
- **Tags live in the text too:** `#tag` in the body (letters, digits,
  `_`, `-`, `/`, not only digits), outside code, as in Obsidian. Format
  1's separate `tags` field is dropped; format-1 tags are appended to the
  body as `#tag` text when read. YAML front matter is shown as text and
  not parsed.
- **Indexes are built, never stored:** at unlock and after every save,
  Crystal builds in zeroizing memory: name to note, title to notes, tag
  to notes, links and backlinks, headings and block ids per note. A few
  milliseconds for thousands of notes.
- **Renames rewrite links:** `mv` changes the target part of every link
  that resolved to the note (keeping `|shown text` and `#fragment`), in
  the same save as the rename. A renamed heading is not rewritten; the
  `links` command and the pager report broken links.
- **`VERSION`** is the newest blob format the directory may hold: unlock
  accepts a `vault.age` whose format is at most `VERSION`. When a save
  first writes format 2 into a format-1 vault, it writes `VERSION` `2`
  before `vault.age`, so a crash in between leaves a readable vault.
  Format 1 is read (empty folder, tags moved into the body) and written
  back as format 2 on the next save.
- **Limits:** as format 1 (100 000 notes, 8 MiB bodies, 32 MiB padded
  vault), plus the folder and name rules above.

## Alternatives

- **One encrypted file per note** (folders as real directories): per-note
  sync conflicts and smaller saves, but the storage shows the note count,
  each note's size and which note changed when. Rejected; see
  [0001](0001-single-vault-blob.md).
- **A separate encrypted metadata file:** two files to keep consistent
  through crashes, partial syncs and rollbacks.
- **A stored links table (note ids and positions):** survives renames,
  but positions move with every edit and merge, links are invisible in
  the text, and an export loses them.
- **CSV records:** quoting and escaping for multi-line Markdown, no
  length framing, a new parser for untrusted input.
- **Keeping a `tags` field:** a second place for tags besides the
  Obsidian `#tag` syntax.

## Consequences

- Notes export one-to-one to an Obsidian vault (`folder/title.md`), and
  an Obsidian vault can be imported, both in a later phase.
- Renaming is a whole-vault text rewrite; for text notes that is
  milliseconds.
- Link and tag parsing runs over untrusted text: it is part of the
  renderer fuzzing in Phase 3.
- A golden-bytes test pins format 2; format 1's golden test stays for
  reading.
