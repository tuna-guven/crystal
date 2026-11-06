# Overview and goals

Crystal is a single static Rust binary that opens an age-encrypted Markdown vault in a full-screen TUI, decrypts notes only into locked memory, and never writes plaintext to disk.

**Goals**

- Obsidian-like core: a vault of Markdown notes, `[[wikilinks]]`, backlinks, tags, a fuzzy note finder and full-text search
- Two modes per note: rendered view and in-process editor
- One static musl binary, installed per user in `~/.local/bin` without root, no runtime dependencies, no network code at all
- Confidentiality and integrity of notes and their titles at rest, including against whoever stores or syncs the files

**Non-goals (v1)**

- Plugins, scripting, themes beyond a fixed palette
- Sync: Syncthing or git moves ciphertext; Crystal only detects tampering and conflicts
- Images, attachments, HTML, embeds, link opening
- Mobile, multi-user vaults, collaborative editing

**Design principles**

1. Plaintext lives only in mlocked, zeroized memory inside one process.
2. Fail closed: any integrity mismatch refuses to open, never "repairs" silently.
3. Boring crypto: age only, no custom primitives, no custom KDF.
4. Deny by default: Landlock and seccomp shrink what a compromised process can do; an SELinux policy module and an optional bubblewrap launcher add outer layers ([Sandboxing](sandboxing.md)).
5. Every dependency must justify itself; the code you write is small enough to review end to end.
