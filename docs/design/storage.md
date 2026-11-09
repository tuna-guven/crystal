# Storage: layout, formats and keys

## On-disk layout

Vaults live in one fixed place, `~/.local/share/crystal/NAME`, and are opened by name ([decision 0032](../decisions/0032-fixed-locations.md)). There is no config file and no option to put a vault elsewhere; to sync or back up a vault, point Syncthing or restic at its directory.

```text
~/.local/share/crystal/        mode 0700; created by the first crystal init
  NAME/                        one vault, mode 0700
    VERSION                    plaintext: the newest blob format in this directory
    identity.age               vault X25519 identity, encrypted to the vault's passphrase or hardware key(s)
    identity.backup.age        the same identity, encrypted to the recovery code
    vault.age                  the entire vault: every note, padded and encrypted
~/.local/state/crystal/        mode 0700; local only, never synced or backed up
  NAME.pin                     the vault's vault_id, pinned at init or first unlock
  <vault_id>.gen               last seen vault generation
~/.config/crystal/             settings later (plain text, written by you, read by Crystal)
```

## Vault directory

- *Finding a vault:* `HOME` is the only input (startup step 3). `~/.local/share/crystal` is reached from `HOME` one component at a time with `O_NOFOLLOW`, so a symlink anywhere is refused; `.local` and `share` must be yours and not group- or world-writable; `crystal` and the vault directory must be yours and mode 0700. The vault directory must hold a valid [`VERSION`](#version). NAME follows the vault name rules (startup step 6), so it can never be a path.
- *Listing:* `crystal` with no argument, and completion, list the directories in `~/.local/share/crystal`.
- *Pin* (`NAME.pin`): 64 lowercase hex digits of the `vault_id` and a newline, a fixed public format written by the atomic writer ([decision 0012](../decisions/0012-public-bytes.md)). `crystal init` writes it; on a device without it (a vault synced or restored here), the first unlock writes it and says so. Unlock refuses a vault whose `vault_id` differs from the pin, so one of your vaults cannot be swapped in for another under its name.
- *One passphrase per vault:* the one that seals its `identity.age`. Unlock runs scrypt once.
- *`crystal recover NAME`:* asks for the recovery code, opens `identity.backup.age` and `vault.age` with it (a wrong code or another vault's backup is refused first), checks the pin and the rollback counter, then asks for a new passphrase twice and rewrites `identity.age` only ([decision 0029](../decisions/0029-recovery-code.md)).
- *`crystal check-code NAME`:* asks for the recovery code and the passphrase, and checks that `identity.backup.age` and `identity.age` hold the same identity. It writes nothing.
- *`crystal rm NAME`:* asks for the name again, says that only a backup can bring the vault back, then removes `VERSION`, `identity.age`, `identity.backup.age`, `vault.age`, `vault.sync-conflict-*.age`, leftover atomic-writer temp files, the directory, and the vault's `.pin` and `.gen`. Any other file stays, and so does the directory, and the message says so. There is no trash ([decision 0032](../decisions/0032-fixed-locations.md)).
- *SELinux:* the policy labels `~/.local/share/crystal` statically; every vault is covered ([Sandboxing](sandboxing.md)).
- *Settings:* `~/.config/crystal` will hold a small plain-text settings file (auto-lock time). Crystal reads it and never writes it, and it holds nothing that loosens security. It is added when the first setting needs it (Phase 5).

## Key design

`crystal init` generates a random vault X25519 identity and stores it twice. `identity.age` holds it for daily unlock, encrypted either to the vault's scrypt passphrase alone or to one or more YubiKey or TPM plugin recipients. `identity.backup.age` holds it for recovery, scrypt-encrypted to a recovery code that Crystal generates and the user writes on paper ([decision 0029](../decisions/0029-recovery-code.md)). Two files are needed because age accepts a scrypt stanza only as the sole recipient of a file, so the passphrase and the code cannot share one. `vault.age` is encrypted to the vault recipient. Changing the passphrase or adding a hardware key rewrites only `identity.age`; a new recovery code rewrites only `identity.backup.age`.

**`crystal init NAME`** (`init.rs`, [decision 0022](../decisions/0022-init.md), [decision 0032](../decisions/0032-fixed-locations.md)) creates nothing until everything is known:

1. `~/.local/share/crystal` is opened, or created (0700) if missing, with the checks under [Vault directory](#vault-directory); `NAME` must not exist in it, not even as a symlink.
2. A new passphrase is asked for twice (at least 20 characters; [decision 0028](../decisions/0028-passphrase-minimum.md)).
3. A recovery code is generated (27 random Crockford base32 symbols and a check symbol), shown once as 7 groups of 4, and must be typed back before anything is created.
4. Created, in order: the vault directory (0700), `VERSION`, a new identity and random `vault_id`, `identity.age`, `identity.backup.age`, and an empty `vault.age` at generation 1 (each 0600, through the atomic writer). The finished directory must pass the full vault check; then `NAME.pin` and the rollback counter are written.

If a step in 4 fails, the files, the vault directory and the pin are removed again, and the error says so. `~/.local/share/crystal` itself is created before the prompts, like the state directory, and stays; a directory that existed before is never removed.

## Vault contents

The decrypted vault is a small database of note records: postcard, then Padmé-padded and encrypted. Folders are metadata, and links and tags are read from the text ([decision 0034](../decisions/0034-vault-format-2.md)).

**Format 2** (`vault::codec`, written since step L2; name rules in `model::name`). Postcard writes integers as varints, and strings and lists with a varint length first:

```text
format: u16 = 2 | vault_id: [u8; 32] | generation: u64 | notes: [Note]
Note = id: [u8; 16] | folder: str | title: str | created: u64 | modified: u64 | body: str
```

- *Names:* a note's name is `folder/title`, or `title` when `folder` is empty. `folder` is segments joined by `/`, at most 32 levels and 1 KiB. A title or folder segment is 1 to 255 bytes, has no leading or trailing space, is not `.` or `..`, and contains none of `[ ] # ^ | \ / : * ? " < >`, no control or BiDi character and no invisible one such as a zero-width space ([decision 0040](../decisions/0040-invisible-name-characters.md)). Names are unique per vault, compared case-insensitively. These are Obsidian's rules, and valid file names for an export.
- *Links:* Obsidian syntax in the body (followed in the pager, `view`; broken ones listed by `links`): `[[name]]`, `[[name|shown text]]`, `[[name#Heading]]`, `[[name#^block-id]]`, `[[#Heading]]`. A target without `/` matches a unique title. A block id `^id` ends a paragraph or list item. `mv` rewrites links to the renamed note in the same save; a broken link is reported, never silently changed.
- *Tags:* `#tag` in the body outside code (letters, digits, `_`, `-`, `/`, not only digits).
- *Plain text:* a note whose title ends in `.txt` is shown as written, with no Markdown, links or tags read from it ([decision 0038](../decisions/0038-plain-text-notes.md)); every other note is Markdown.
- *Indexes:* name, title, tag, links, backlinks, headings and block ids are built in zeroizing memory at unlock and after every save, never stored.
- *Format 1* ([decision 0018](../decisions/0018-vault-format-1.md)), written by Crystal before step L2, has no `folder` and a separate `tags: [str]` field after `title`. It is still read: the folder is empty and its tags are appended to the body as `#tag`, after an empty line. Its notes must meet format 2's name rules (non-empty titles without `/` and the other reserved characters, unique ignoring case); otherwise the vault is refused, never renamed silently. The next save writes format 2.

`created` and `modified` are seconds since the Unix epoch. The format number is decoded on its own first, so an unknown format is refused before anything else is parsed. A golden-bytes test pins each format's exact encoding.

**Limits**, enforced when decoding and when encoding, so Crystal never writes a vault it would refuse to read:

| Limit | Value |
| --- | --- |
| Notes per vault | 100 000 |
| Title, folder segment | 255 bytes |
| Folder | 32 levels, 1 KiB |
| Body | 8 MiB |
| Whole vault, padded | 32 MiB (`crypto::MAX_PLAINTEXT`) |

Note ids and names must be unique, and no bytes may follow the last note. Limits are checked before any note text is copied out of the decrypted buffer.

**Pipeline.** `vault::format::seal` is encode, then Padmé pad, then encrypt to the vault recipient; `open` is the reverse. Every plaintext stage lives in a zeroizing buffer of exact size. age authenticates the whole file, so a changed byte anywhere fails at decryption, before padding or contents are looked at.

**Size on disk.** The encrypted payload is exactly the Padmé frame plus age's nonce and 16-byte tag per 64 KiB chunk. The header around it holds the recipient stanza and a "grease" stanza that age adds with a random size of up to about 200 bytes, so the file size is the Padmé bucket plus a small random amount, unrelated to the notes.

## VERSION

The vault directory's `VERSION` file holds the newest blob format the directory may contain; unlock accepts a `vault.age` whose format is at most that number ([decision 0034](../decisions/0034-vault-format-2.md)). A save that first writes a newer format writes `VERSION` before `vault.age`, so a crash in between leaves a readable vault (the save comes in Phase 4; `crystal init` writes `2`). A `vault.age` in a newer format than `VERSION` allows is refused after decryption. The file holds the format number in ASCII decimal and one newline (`2\n`): no sign, no leading zero, no `\r`, spaces or second line. It is plaintext by design ([decision 0012](../decisions/0012-public-bytes.md)) and is read first at unlock, so an unknown format is refused before anything is decrypted. `vault::format::read_version` opens it with `O_NOFOLLOW` and `O_NONBLOCK` (a FIFO cannot block the open), refuses anything but a regular file, and reads at most seven bytes whatever the file's size. A directory without `VERSION` is not a vault. `crystal init` writes it through the atomic writer.

## Integrity

One age stream authenticates every note at once, so swapping, injecting or deleting single notes is impossible by construction. The embedded `vault_id`, checked against `NAME.pin`, rejects a vault from elsewhere, and the local generation file catches an older copy (rollback).

## Rollback

age proves a file is authentic, not that it is the newest. Someone who controls the storage or sync (A1) could put back an older `vault.age`, undoing edits. Each file therefore carries a `generation` (+1 per write), and the state directory keeps the last one this device has seen ([decision 0019](../decisions/0019-generation-counters.md)):

| File | Counter |
| --- | --- |
| `vault.age` | `~/.local/state/crystal/<vault_id as 64 lowercase hex>.gen` |

- **Format:** the generation in ASCII decimal and one newline (`42\n`), with exactly one valid spelling, the same rules as `VERSION`. Written through the atomic writer (`PublicBytes::generation`), mode 0600.
- **On open** (`vault::generation::accept_on_open`), after a successful decrypt: equal or higher is accepted and the counter set to it. No counter means the first unlock on this device; rollback detection starts from there, and the user is told.
- **Older vault: restore or attack** ([decision 0036](../decisions/0036-backups-external.md); `generation::open_or_older`, `restore`): a generation lower than the counter stops the unlock, and `crystal recover`, with both numbers and an explanation: the vault was restored from a backup, or someone put back an old copy. Typing `restore` accepts it and sets the counter to the vault's generation; anything else exits and changes nothing. The text says not to type `restore` unless you restored the vault yourself. This is the only way a counter is lowered.
- **On save** (`record_save`): the counter is written after the rename of the new file has committed. A crash in between leaves the counter one behind the file, which the next open accepts and catches up. A saved generation below the counter (another writer got ahead) is reported, and the counter is not lowered.
- **Fail closed:** a counter that is not exactly a decimal number and a newline, or is not a regular file, is refused. Crystal never resets a counter it cannot read; the user must look at it.
- **Limits:** detection is relative to what *this device* has seen. A fresh device, or one whose state directory was lost, accepts whatever it sees first. The counters are plaintext, so a process running as you can lower or delete them (A5); this is accepted, see the decision record.

## Save sequence

Serialize, pad, encrypt to a temp file, `fsync`, rename over `vault.age`, `fsync` the directory. The rename is the commit point; a crash leaves either the old vault or the new one, never a mix. Then the vault's generation counter is updated ([Rollback](#rollback)).

## Costs of one blob, accepted

Every save re-encrypts the whole vault (milliseconds for a few MB of text); all notes sit decrypted in locked memory while unlocked, so auto-lock matters more; a Syncthing conflict produces a second `vault.sync-conflict-*.age`, which Crystal decrypts at unlock and merges per note, flagging notes edited on both sides.

## Open questions

- Stronger scrypt by choice: let a user ask for more scrypt memory than the fixed log N 18 when creating a vault or changing the passphrase, for example `--scrypt-memory 16G` (log N 24). Measured on a 32 GB desktop in a release build: 0.5 s at log N 18 (256 MiB), 2 s at 20 (1 GiB), 8 s at 22 (4 GiB), so about 33 s at 24. Unlock runs scrypt once (`identity.age`), so about half a minute per unlock, about 17 GiB of locked memory, and every device that opens the vault needs the same. The decryption cap would then become "memlock limit minus 128 MiB" instead of half the limit. Deferred on 2026-11-06; since 2026-11-07 every passphrase file uses log N 18 on every device ([decision 0027](../decisions/0027-scrypt-fixed.md)). Revisit with passphrase change in Phase 8.
- Post-quantum: check whether the `age` crate version you pin supports a hybrid ML-KEM recipient type. If it does, use it for the vault recipient, since synced ciphertext is exposed to harvest-now, decrypt-later. Go age 1.3+ has native ML-KEM-768 + X25519 recipients; until rage ships the same type natively, avoid unaudited third-party PQ crates, and avoid PQ via a plugin, since notes are encrypted on every save and plugins need execve.
  - *Answer (2026-11-06, #4):* not yet; the vault recipient stays X25519. The pinned `age` 0.11.5 has only X25519, scrypt and plugin recipients. The newer 0.12 line adds `tagpq` (`mlkem768p256tag`), but it is encryption-only: decrypting needs a hardware tag through a plugin, so it cannot be the vault's own recipient. 0.12 also pulls in `p256` and `crypto-bigint` 0.5, which has a timing advisory (RUSTSEC-2026-0387), for that feature alone. Neither line has a native hybrid ML-KEM-768 + X25519 identity like Go age 1.3's. Revisit when rage ships one: upgrade `age` and re-encrypt the vault to the new recipient on the next save. See [decision 0003](../decisions/0003-no-post-quantum-yet.md).
- Per-note files versus one encrypted vault blob: decided, one vault blob ([decision 0001](../decisions/0001-single-vault-blob.md), confirmed by [decision 0034](../decisions/0034-vault-format-2.md)). It hides note count, sizes and which note changed, at the cost of whole-vault conflicts and all notes in memory while unlocked. A separate metadata file was rejected for the same reason as a stored link table: a second source of truth.
