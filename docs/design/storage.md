# Storage: layout, formats and keys

## On-disk layout

Vaults are opened by name. An encrypted per-device config file, `~/.config/crystal/config.age`, maps each name to a vault directory and holds other metadata. `crystal init NAME` creates the vault in `~/Vaults/<name>` by default. With `--at DIR` it can go elsewhere, for example a synced folder. Every path in the config is checked at startup as if it were attacker-written (see [Config file](#config-file)), so the config can point Crystal at a vault but cannot widen the sandbox beyond one checked, private directory.

```text
~/.config/crystal/            mode 0700, local only, never synced
  config.age     vault names, paths, pinned vault_ids, settings; padded and encrypted
~/Vaults/<name>/              default vault location, mode 0700 (any checked path via --at)
  VERSION        plaintext: format version only
  identity.age   vault X25519 identity, encrypted to your passphrase or to your hardware key(s)
  identity.backup.age  the same identity, encrypted to the recovery code
  vault.age      the entire vault: every note, title, link and tag, padded and encrypted
~/.local/state/crystal/<vault_id>.gen   last seen vault generation (local only, never synced)
~/.local/state/crystal/config.gen       last seen config generation
```

## Config file

The config is local to one device, because paths differ between machines; a vault directory stays self-contained and syncable without it. If the config is lost, nothing is lost: `crystal add NAME DIR` registers an existing vault again.

- *Contents (postcard, then Padmé-padded, then age):* format version, `generation` (u64, +1 per write), default `lock_after`, and per vault: name, absolute path, pinned `vault_id`, optional `lock_after`; from Phase 7, the backup drive markers. It holds nothing that loosens security: no sandbox switches, no executable paths, no editor, no environment.
- *Format 1* (`config.rs`, [decision 0021](../decisions/0021-config-format.md)):

  ```text
  format: u16 | generation: u64 | lock_after: Option<u32> | vaults: [Entry]
  Entry = name: str | path: str | vault_id: [u8; 32] | lock_after: Option<u32>
  ```

  Decoding refuses an unknown format, trailing bytes, more than 256 vaults, an invalid or repeated name, a path that is not absolute clean UTF-8 (no empty, `.` or `..` part), and a `lock_after` outside 1 to 1440 minutes; encoding applies the same rules.
- *Pinning:* each entry's `vault_id` is read from the vault itself (its `vault.age`, opened with the passphrase) when it is registered by `crystal init` or `crystal add`, so a normal session never needs to write the config.
- *Encryption:* encrypted with the `age` crate, never by shelling out to `rage`, to one unlock method only. The choices are a scrypt passphrase or a hardware-key plugin recipient. age allows a scrypt stanza only as the sole recipient, and the config needs no recovery code because it can be rebuilt. With a passphrase, Crystal prompts once. It decrypts `config.age`, then the vault's `identity.age`, with the same passphrase held in a `Zeroizing` buffer, then wipes the buffer. With a plugin, each file costs one touch.
- *Integrity limits:* age authenticates the content but not who wrote it. Under a scrypt passphrase, only the passphrase holder can produce a config that decrypts. Under a public-key or plugin recipient, anyone who knows the recipient can write a valid `config.age`. So Crystal never trusts the config for a security decision. Each vault path must be absolute, with no `..`. It is opened one component at a time from `/` with `O_NOFOLLOW`. Every directory on the way must be owned by root or by you and must not be group- or world-writable. The vault directory itself must be yours, mode 0700, and contain `VERSION`. It must not be `$HOME`, an ancestor of the config or state directory, or inside either. The pinned `vault_id` detects a swapped vault directory, but only as strongly as the config is authenticated.
- *Vault path checks* (`vault::VaultDir::open_checked`, [decision 0020](../decisions/0020-vault-path-checks.md)), in this order:
  1. The path's raw bytes: absolute, at most PATH_MAX, no NUL, no empty, `.` or `..` component. (`Path::components` would silently drop `.` and `//`, so two spellings could pass as one.)
  2. A walk from `/`, one component at a time with `O_NOFOLLOW`, so a symlink anywhere is refused. Paths in the config must therefore be fully resolved: on Fedora Atomic, `/var/home/you/Vaults/x`, not `/home/you/...`.
  3. Every directory on the way is owned by root or by you and is not group- or world-writable. There is no exception for sticky directories such as `/tmp`.
  4. No directory on the way, nor the vault itself, is the config or state directory; the vault is not `$HOME`; and it is not an ancestor of the config or state directory (found by opening `..` upwards from their fds). All of these compare `(st_dev, st_ino)` of open fds, never strings, so spellings, symlinked `HOME` and bind mounts cannot trick them.
  5. The vault directory is yours, mode 0700, and holds a valid [`VERSION`](#version).

  Errors name the position of the failing component, never the path.
- *Rollback:* `config.gen` in the state directory rejects an older config, the same way as for vaults ([Rollback](#rollback)).
- *Writes:* only `crystal init`, `crystal add` and `crystal remove` rewrite the config, through `vault::atomic_write`, and then exit. A normal session reads the config before the sandbox and has no write access to `~/.config/crystal` afterwards.
- *`crystal add NAME DIR`:* resolves DIR to its real path (the config stores fully resolved paths, [decision 0020](../decisions/0020-vault-path-checks.md)), runs the vault path checks, and only then asks for a passphrase: the existing config's, or, on a device without a config, a new one entered twice. With that passphrase it opens the vault's `identity.age` and `vault.age` to pin the `vault_id`, starts the vault's rollback counter, adds the entry and stores the config. A bad path, a taken name or a passphrase that does not open the vault writes nothing.
- *`crystal recover NAME DIR`:* checks DIR as `add` does, then asks for the recovery code and opens `identity.backup.age` and `vault.age` with it. Then it asks for the device passphrase: given, the existing config is kept and the vault registered in it; Enter (forgotten), after the word `replace`, a new passphrase and a new config with only this vault; no config, a new passphrase. After the name and the rollback counter are checked, `identity.age` is re-sealed to the passphrase and written first, the config last. A wrong code, a vault that does not match its backup, a rollback or a taken name writes nothing ([decision 0029](../decisions/0029-recovery-code.md)).
- *`crystal remove NAME`:* unlocks the config, drops the entry and stores the config. The vault's files are never touched; without a config, it fails like a normal run.
- *SELinux:* the policy labels `~/Vaults` statically. A vault at any other path needs a one-time `semanage fcontext` rule ([Sandboxing](sandboxing.md)). Without the rule, that vault runs without the SELinux layer, and `--sandbox-status` says so.

## Key design

`crystal init` generates a random vault X25519 identity and stores it twice. `identity.age` holds it for daily unlock, encrypted either to a scrypt passphrase alone or to one or more YubiKey or TPM plugin recipients. `identity.backup.age` holds it for recovery, scrypt-encrypted to a recovery code that Crystal generates and the user writes on paper ([decision 0029](../decisions/0029-recovery-code.md)). Two files are needed because age accepts a scrypt stanza only as the sole recipient of a file, so the passphrase and the code cannot share one. `vault.age` is encrypted to the vault recipient. Changing the passphrase or adding a hardware key rewrites only `identity.age`; a new recovery code rewrites only `identity.backup.age`.

**`crystal init NAME [--at DIR]`** (`init.rs`, [decision 0022](../decisions/0022-init.md)) creates nothing until everything is known:

1. The target is resolved: `~/Vaults/NAME` (with `HOME` resolved), or DIR itself with `--at`. It must not exist, and its parent (or, for a missing `~/Vaults`, HOME) must pass the vault path walk and be a safe parent itself.
2. The passphrase is asked for: the config's, or a new one twice on a device without a config. A name already in the config is refused here.
3. A recovery code is generated (27 random Crockford base32 symbols and a check symbol), shown once as 7 groups of 4, and must be typed back before anything is created.
4. Created, in order: `~/Vaults` (0700) if missing, the vault directory (0700), `VERSION`, a new identity and random `vault_id`, `identity.age`, `identity.backup.age`, and an empty `vault.age` at generation 1 (each 0600, through the atomic writer). The finished directory must pass the full vault check; then the entry is added to the config, the config stored, and the vault's rollback counter set to 1.

If a step in 4 fails, the files, the vault directory and a `~/Vaults` made by this run are removed again, and the error says so. A directory that existed before is never removed. The exception is a failure once the new `config.age` has been renamed into place (the commit point): writing `config.gen`, or the directory fsync after that rename. The vault is then complete and registered, so it stays.

## Vault contents

Postcard, then Padmé-padded and encrypted: format version, `vault_id` (32 random bytes), `generation` (u64, +1 per save), and every note with its id, title, tags, created, modified and Markdown body.

**Format 1** (`vault::codec`, [decision 0018](../decisions/0018-vault-format-1.md)). Postcard writes integers as varints, and strings and lists with a varint length first:

```text
format: u16 | vault_id: [u8; 32] | generation: u64 | notes: [Note]
Note = id: [u8; 16] | title: str | tags: [str] | created: u64 | modified: u64 | body: str
```

`created` and `modified` are seconds since the Unix epoch. The format number is decoded on its own first, so an unknown format is refused before anything else is parsed. A golden-bytes test pins the exact encoding: any change to it needs a new format number.

**Limits**, enforced when decoding and when encoding, so Crystal never writes a vault it would refuse to read:

| Limit | Value |
| --- | --- |
| Notes per vault | 100 000 |
| Title | 1 KiB |
| Tags per note | 64 |
| Tag | 256 bytes |
| Body | 8 MiB |
| Whole vault, padded | 32 MiB (`crypto::MAX_PLAINTEXT`) |

Note ids must be unique, and no bytes may follow the last note. Limits are checked before any note text is copied out of the decrypted buffer.

**Pipeline.** `vault::format::seal` is encode, then Padmé pad, then encrypt to the vault recipient; `open` is the reverse. Every plaintext stage lives in a zeroizing buffer of exact size. age authenticates the whole file, so a changed byte anywhere fails at decryption, before padding or contents are looked at.

**Size on disk.** The encrypted payload is exactly the Padmé frame plus age's nonce and 16-byte tag per 64 KiB chunk. The header around it holds the recipient stanza and a "grease" stanza that age adds with a random size of up to about 200 bytes, so the file size is the Padmé bucket plus a small random amount, unrelated to the notes.

## VERSION

The vault directory's `VERSION` file holds the format number in ASCII decimal and one newline (`1\n`): no sign, no leading zero, no `\r`, spaces or second line. It is plaintext by design ([decision 0012](../decisions/0012-public-bytes.md)) and is read first at unlock, so an unknown format is refused before anything is decrypted. `vault::format::read_version` opens it with `O_NOFOLLOW` and `O_NONBLOCK` (a FIFO cannot block the open), refuses anything but a regular file, and reads at most seven bytes whatever the file's size. A directory without `VERSION` is not a vault. `crystal init` writes it through the atomic writer.

## Integrity

One age stream authenticates every note at once, so swapping, injecting or deleting single notes is impossible by construction. The embedded `vault_id` rejects a vault from elsewhere, and the local generation file rejects an older copy (rollback).

## Rollback

age proves a file is authentic, not that it is the newest. Someone who controls the storage or sync (A1) could put back an older `vault.age` or `config.age`, undoing edits or a passphrase change. Each file therefore carries a `generation` (+1 per write), and the state directory keeps the last one this device has seen ([decision 0019](../decisions/0019-generation-counters.md)):

| File | Counter |
| --- | --- |
| `vault.age` | `~/.local/state/crystal/<vault_id as 64 lowercase hex>.gen` |
| `config.age` | `~/.local/state/crystal/config.gen` |

- **Format:** the generation in ASCII decimal and one newline (`42\n`), with exactly one valid spelling, the same rules as `VERSION`. Written through the atomic writer (`PublicBytes::generation`), mode 0600.
- **On open** (`vault::generation::accept_on_open`), after a successful decrypt: a generation lower than the counter is refused with a message naming both numbers; equal or higher is accepted and the counter set to it. No counter means the first unlock on this device; rollback detection starts from there, and the user is told.
- **On save** (`record_save`): the counter is written after the rename of the new file has committed. A crash in between leaves the counter one behind the file, which the next open accepts and catches up. A saved generation below the counter (another writer got ahead) is reported, and the counter is never lowered.
- **Lost config:** `config.gen` outlives a deleted or lost `config.age`. A new config made by `crystal init` or `crystal add` on such a device starts at the stored generation, so its first write is one above the counter and is accepted (`config::fresh`).
- **Fail closed:** a counter that is not exactly a decimal number and a newline, or is not a regular file, is refused. Crystal never resets a counter it cannot read; the user must look at it.
- **Limits:** detection is relative to what *this device* has seen. A fresh device, or one whose state directory was lost, accepts whatever it sees first. The counters are plaintext, so a process running as you can lower or delete them (A5); this is accepted, see the decision record.

## Save sequence

Serialize, pad, encrypt to a temp file, `fsync`, rename over `vault.age`, `fsync` the directory. The rename is the commit point; a crash leaves either the old vault or the new one, never a mix. Then the vault's generation counter is updated ([Rollback](#rollback)).

## Costs of one blob, accepted

Every save re-encrypts the whole vault (milliseconds for a few MB of text); all notes sit decrypted in locked memory while unlocked, so auto-lock matters more; a Syncthing conflict produces a second `vault.sync-conflict-*.age`, which Crystal decrypts at unlock and merges per note, flagging notes edited on both sides.

## Open questions

- Stronger scrypt by choice: let a user ask for more scrypt memory than the fixed log N 18 when creating a vault or changing the passphrase, for example `--scrypt-memory 16G` (log N 24). Measured on a 32 GB desktop in a release build: 0.5 s at log N 18 (256 MiB), 2 s at 20 (1 GiB), 8 s at 22 (4 GiB), so about 33 s at 24. Unlock runs scrypt twice (`config.age`, `identity.age`), so about a minute per unlock, about 17 GiB of locked memory, and every device that opens the vault needs the same. The decryption cap would then become "memlock limit minus 128 MiB" instead of half the limit. Deferred on 2026-11-06; since 2026-11-07 every passphrase file uses log N 18 on every device ([decision 0027](../decisions/0027-scrypt-fixed.md)). Revisit with passphrase change in Phase 8.
- Post-quantum: check whether the `age` crate version you pin supports a hybrid ML-KEM recipient type. If it does, use it for the vault recipient, since synced ciphertext is exposed to harvest-now, decrypt-later. Go age 1.3+ has native ML-KEM-768 + X25519 recipients; until rage ships the same type natively, avoid unaudited third-party PQ crates, and avoid PQ via a plugin, since notes are encrypted on every save and plugins need execve.
  - *Answer (2026-11-06, #4):* not yet; the vault recipient stays X25519. The pinned `age` 0.11.5 has only X25519, scrypt and plugin recipients. The newer 0.12 line adds `tagpq` (`mlkem768p256tag`), but it is encryption-only: decrypting needs a hardware tag through a plugin, so it cannot be the vault's own recipient. 0.12 also pulls in `p256` and `crypto-bigint` 0.5, which has a timing advisory (RUSTSEC-2026-0387), for that feature alone. Neither line has a native hybrid ML-KEM-768 + X25519 identity like Go age 1.3's. Revisit when rage ships one: upgrade `age` and re-encrypt the vault to the new recipient on the next save. See [decision 0003](../decisions/0003-no-post-quantum-yet.md).
- Per-note files versus one encrypted vault blob: decided, one vault blob. It hides note count, sizes and which note changed, at the cost of whole-vault conflicts and all notes in memory while unlocked.
