# 0001 One encrypted vault blob

- **Date:** 2026-10-11
- **Status:** Accepted

## Context

Notes could be stored one file per note, or all together in one file.
Per-note files leak the note count, each note's size, and which note
changed on every save, to anyone who sees the storage or the sync traffic
(threat A1). They also need a separate scheme to stop single notes from
being swapped, removed or rolled back.

## Decision

The whole vault (every note, title, link, tag and timestamp) is one
postcard-encoded, Padmé-padded, age-encrypted file, `vault.age`. One age
stream authenticates all notes at once. See
[Storage](../design/storage.md).

## Alternatives

- **Per-note files with encrypted names:** leaks count, sizes and change
  patterns; integrity of the set needs extra machinery.
- **An encrypted filesystem (gocryptfs) plus an editor:** plaintext then
  exists in the page cache, editor swap files and temp files.

## Consequences

- Note count, sizes and which note changed stay hidden; only the Padmé
  size bucket and the save time leak.
- Swapping, injecting or deleting single notes is impossible by
  construction.
- Every save re-encrypts the whole vault (milliseconds for a few MB).
- All notes sit decrypted in locked memory while unlocked, so auto-lock
  matters more.
- A sync conflict produces a second whole vault, which Crystal must merge
  per note (Phase 4).
