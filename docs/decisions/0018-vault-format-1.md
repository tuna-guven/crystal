# 0018 Vault format 1

- **Date:** 2026-11-06 (#10)
- **Status:** Accepted; the note layout and the `VERSION` rule are
  superseded by [0034](0034-vault-format-2.md) (format 2)

## Context

`vault.age` needs a byte format that is compact, unambiguous, versioned,
and safe to parse after decryption even if the plaintext was crafted by
someone who holds the vault recipient (anyone can encrypt to a public
key). The format number must be readable before unlocking, so a vault
from a newer Crystal is refused cleanly instead of failing deep inside
parsing.

## Decision

- **Encoding:** postcard, in the fixed field order `format (u16) |
  vault_id | generation | notes`, each note `id | title | tags | created
  | modified | body` ([Storage, Vault contents](../design/storage.md#vault-contents)).
  The format number is decoded alone first; unknown numbers are refused
  before the rest is parsed. A golden-bytes test pins the encoding.
- **Limits:** at most 100 000 notes, 1 KiB titles, 64 tags of 256 bytes
  each, 8 MiB bodies, unique note ids, no trailing bytes. Checked by
  decode before any text is copied, and by encode, so Crystal never
  writes a vault it would refuse to read.
- **`VaultContents` has no format field:** encode always writes the
  current format, so no caller can produce an unknown one.
- **`VERSION`:** the same number as ASCII decimal plus one newline, with
  exactly one valid spelling per number. Read first at unlock, from a
  regular file only (`O_NOFOLLOW`, `O_NONBLOCK`, `fstat`), at most seven
  bytes. Written by `--init` through the atomic writer; its encoder is the
  first of the two `PublicBytes` constructors ([0012](0012-public-bytes.md)).
- **Pipeline:** `seal` = encrypt(pad(encode)), `open` = decode(unpad(decrypt)).

## Alternatives

- **A self-describing format (CBOR, JSON):** larger, more parser surface,
  and several encodings for the same value.
- **Format number only inside the encrypted blob:** an unknown format
  would only be noticed after unlock, with a less helpful error.
- **No limits:** a crafted vault could make Crystal copy and index huge
  strings or millions of notes.

## Consequences

- Any change to the field order, a type or a limit needs format 2 and a
  migration on the next save.
- The limits are part of the format: raising one later means a new format
  number, even though old vaults stay readable.
- The ciphertext size is the Padmé bucket plus a small random amount:
  age adds a "grease" stanza of random size to every header that is not
  a passphrase file. It leaks nothing about the notes. age draws it from
  `rand`'s `thread_rng`, seeded from the same `getrandom` syscall
  ([0015](0015-age-getrandom.md)).
