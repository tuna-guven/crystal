# Storage: layout, formats and keys

## On-disk layout

Vaults are opened by name. An encrypted per-device config file, `~/.config/crystal/config.age`, maps each name to a vault directory and holds other metadata. `crystal --init NAME` creates the vault in `~/Vaults/<name>` by default. With `--at DIR` it can go elsewhere, for example a synced folder. Every path in the config is checked at startup as if it were attacker-written (see [Config file](#config-file)), so the config can point Crystal at a vault but cannot widen the sandbox beyond one checked, private directory.

```text
~/.config/crystal/            mode 0700, local only, never synced
  config.age     vault names, paths, pinned vault_ids, settings; padded and encrypted
~/Vaults/<name>/              default vault location, mode 0700 (any checked path via --at)
  VERSION        plaintext: format version only
  identity.age   vault X25519 identity, encrypted to your passphrase or to your hardware key(s)
  identity.backup.age  the same identity, encrypted to the offline backup recipient(s)
  vault.age      the entire vault: every note, title, link and tag, padded and encrypted
~/.local/state/crystal/<vault_id>.gen   last seen vault generation (local only, never synced)
~/.local/state/crystal/config.gen       last seen config generation
```

## Config file

The config is local to one device, because paths differ between machines; a vault directory stays self-contained and syncable without it. If the config is lost, nothing is lost: `crystal --add NAME DIR` registers an existing vault again.

- *Contents (postcard, then Padmé-padded, then age):* format version, `generation` (u64, +1 per write), default `lock_after`, and per vault: name, absolute path, pinned `vault_id` (filled in at first unlock), optional `lock_after`; from Phase 7, the backup drive markers. It holds nothing that loosens security: no sandbox switches, no executable paths, no editor, no environment.
- *Encryption:* encrypted with the `age` crate, never by shelling out to `rage`, to one unlock method only. The choices are a scrypt passphrase or a hardware-key plugin recipient. age allows a scrypt stanza only as the sole recipient, and the config needs no backup recipient because it can be rebuilt. With a passphrase, Crystal prompts once. It decrypts `config.age`, then the vault's `identity.age`, with the same passphrase held in a `Zeroizing` buffer, then wipes the buffer. With a plugin, each file costs one touch.
- *Integrity limits:* age authenticates the content but not who wrote it. Under a scrypt passphrase, only the passphrase holder can produce a config that decrypts. Under a public-key or plugin recipient, anyone who knows the recipient can write a valid `config.age`. So Crystal never trusts the config for a security decision. Each vault path must be absolute, with no `..`. It is opened one component at a time from `/` with `O_NOFOLLOW`. Every directory on the way must be owned by root or by you and must not be group- or world-writable. The vault directory itself must be yours, mode 0700, and contain `VERSION`. It must not be `$HOME`, an ancestor of the config or state directory, or inside either. The pinned `vault_id` detects a swapped vault directory, but only as strongly as the config is authenticated.
- *Rollback:* `config.gen` in the state directory rejects an older config, the same way as for vaults.
- *Writes:* only `--init`, `--add` and `--remove` rewrite the config, through `vault::atomic_write`, and then exit. A normal session reads the config before the sandbox and has no write access to `~/.config/crystal` afterwards.
- *SELinux:* the policy labels `~/Vaults` statically. A vault at any other path needs a one-time `semanage fcontext` rule ([Sandboxing](sandboxing.md)). Without the rule, that vault runs without the SELinux layer, and `--sandbox-status` says so.

## Key design

`crystal --init` generates a random vault X25519 identity and stores it twice. `identity.age` holds it for daily unlock, encrypted either to a scrypt passphrase alone or to one or more YubiKey or TPM plugin recipients. `identity.backup.age` holds it for recovery, encrypted to the offline backup recipient(s). Two files are needed because age accepts a scrypt stanza only as the sole recipient of a file, so a passphrase and a backup key cannot share one. `vault.age` is encrypted to the vault recipient. Changing the passphrase or adding a hardware key rewrites only `identity.age`; rotating the backup key rewrites only `identity.backup.age`.

## Vault contents

Postcard, then Padmé-padded and encrypted: format version, `vault_id` (32 random bytes), `generation` (u64, +1 per save), and every note with its id, title, tags, created, modified and Markdown body.

## Integrity

One age stream authenticates every note at once, so swapping, injecting or deleting single notes is impossible by construction. The embedded `vault_id` rejects a vault from elsewhere, and the local generation file rejects an older copy (rollback).

## Save sequence

serialize, pad, encrypt to a temp file, `fsync`, rename over `vault.age`, `fsync` the directory. The rename is the commit point; a crash leaves either the old vault or the new one, never a mix.

## Costs of one blob, accepted

every save re-encrypts the whole vault (milliseconds for a few MB of text); all notes sit decrypted in locked memory while unlocked, so auto-lock matters more; a Syncthing conflict produces a second `vault.sync-conflict-*.age`, which Crystal decrypts at unlock and merges per note, flagging notes edited on both sides.

## Open questions

- Post-quantum: check whether the `age` crate version you pin supports a hybrid ML-KEM recipient type. If it does, use it for the vault recipient, since synced ciphertext is exposed to harvest-now, decrypt-later. Go age 1.3+ has native ML-KEM-768 + X25519 recipients; until rage ships the same type natively, avoid unaudited third-party PQ crates, and avoid PQ via a plugin, since notes are encrypted on every save and plugins need execve.
  - *Answer (2026-11-06, #4):* not yet; the vault recipient stays X25519. The pinned `age` 0.11.5 has only X25519, scrypt and plugin recipients. The newer 0.12 line adds `tagpq` (`mlkem768p256tag`), but it is encryption-only: decrypting needs a hardware tag through a plugin, so it cannot be the vault's own recipient. 0.12 also pulls in `p256` and `crypto-bigint` 0.5, which has a timing advisory (RUSTSEC-2026-0387), for that feature alone. Neither line has a native hybrid ML-KEM-768 + X25519 identity like Go age 1.3's. Revisit when rage ships one: upgrade `age` and re-encrypt the vault to the new recipient on the next save. See [decision 0003](../decisions/0003-no-post-quantum-yet.md).
- Per-note files versus one encrypted vault blob: decided, one vault blob. It hides note count, sizes and which note changed, at the cost of whole-vault conflicts and all notes in memory while unlocked.
