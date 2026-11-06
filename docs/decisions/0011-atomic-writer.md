# 0011 Atomic writer: same-directory temp, renameat2, crash sweep

- **Date:** 2026-11-06 (#7)
- **Status:** Accepted

## Context

A crash or power loss during a save must leave either the old vault or
the new one, never a mix. Landlock's `REFER` right (needed for renames
across directories) should never be granted. A `kill -9` cannot clean up
after itself.

## Decision

`vault::atomic_write` is the only way Crystal writes a file outside
backups:

1. Remove `.<name>.tmp-<16 hex>` files left by an earlier crash.
2. Create `.<name>.tmp-<16 random hex>` in the target's own directory
   (`O_EXCL`, `O_NOFOLLOW`, mode 0600).
3. Write everything, `fsync`.
4. `renameat2` over the target: the commit point. A symlink at the target
   name is replaced, never followed.
5. `fsync` the directory.

On an error before the rename, the temp file is unlinked. rustix's
`renameat_with` is used because plain `renameat` makes the older
`renameat` syscall on x86_64, and the seccomp allowlist has `renameat2`.
Names are a validated `FileName`: one path component, no leading dot.
See [Storage, Save sequence](../design/storage.md#save-sequence).

## Alternatives

- **`O_TMPFILE` and `linkat`:** no name during the write, but `linkat`
  cannot replace an existing file, so a named step is still needed.
- **Leave crash leftovers alone:** a stray temp file stays forever.
- **Temp files in a separate directory:** the rename would need `REFER`.

## Consequences

The sweep lists the directory (`getdents64`) on every write. Tests inject
a failure at every step and `kill -9` a writer in a loop; the target is
always one whole version.
