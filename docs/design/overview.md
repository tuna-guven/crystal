# Overview and goals

Crystal is a single static Rust binary that opens an age-encrypted Markdown vault in a full-screen shell, decrypts notes only into locked memory, and never writes plaintext to disk.

**Goals**

- Obsidian-compatible core: a vault of Markdown notes, and plain text notes, the type chosen when a note is created ([decision 0038](../decisions.md#0038)), in virtual folders, `[[wikilinks]]` to notes, headings and blocks, backlinks, `#tags`, full-text search ([decision 0034](../decisions.md#0034))
- A command shell like gopass's (`ls`, `view`, `search`, `find`, `edit`, `mv`, `rm`), with a full-screen pager and an in-process editor ([decision 0033](../decisions.md#0033))
- Copy and paste: Crystal's own clipboard inside it; copying out is the terminal's own selection ([decision 0041](../decisions.md#0041))
- Vaults in one fixed place, `~/.local/share/crystal`, with no config file ([decision 0032](../decisions.md#0032))
- One static musl binary, installed per user in `~/.local/bin` without root, no runtime dependencies, no network code at all
- Confidentiality and integrity of notes and their titles at rest, including against whoever stores or syncs the files

**Non-goals (v1)**

- Plugins, scripting, themes beyond a fixed palette
- Sync: Syncthing or git moves ciphertext; Crystal only detects tampering and conflicts
- Backups: restic, borg or any file-level tool copies the ciphertext; Crystal only accepts a restored, older vault when asked ([decision 0036](../decisions.md#0036))
- One-shot commands that print notes to stdout, a trash bin
- Images, attachments, HTML, embeds, link opening
- Mobile, multi-user vaults, collaborative editing

**Design principles**

1. Plaintext lives only in mlocked, zeroized memory inside one process.
2. Fail closed: any integrity mismatch refuses to open, never "repairs" silently.
3. Boring crypto: age only, no custom primitives, no custom KDF.
4. Deny by default: Landlock and seccomp shrink what a compromised process can do; an SELinux policy module and an optional bubblewrap launcher add outer layers ([Sandboxing](sandboxing.md)).
5. Minimal and auditable: as few dependencies as possible, and every one must justify itself; build small parts instead of pulling in large crates; the code is small enough to review end to end.
