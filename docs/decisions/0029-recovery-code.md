# 0029 Recovery code instead of a backup recipient

- **Date:** 2026-11-08 (Phase 2 follow-up)
- **Status:** Accepted. Supersedes the backup-recipient part of
  [0022](0022-init.md) and the backup-prompt part of
  [0028](0028-passphrase-minimum.md). The config and DIR parts of
  `recover` are superseded by [0032](0032-fixed-locations.md).

## Context

`crystal init` asked for one or more `age1…` public keys and sealed
`identity.backup.age` to them ([0022](0022-init.md)). Making such a key
needs another program (`age-keygen`, `rage-keygen`) or a second hardware
key. Crystal must be usable with nothing but its own binary; hardening
helpers such as bubblewrap and the SELinux module are the only optional
extras. The prompt was also confusing: users asked why a passphrase was
not enough, and could paste the secret half of the key by mistake.

The risk the backup has to cover is mostly a forgotten passphrase, and
sometimes a lost `identity.age`. Nothing could read `identity.backup.age`
yet, so a backup that did not work would only be found when it was needed.

## Decision

- **Crystal makes the code.** `crystal init` generates a recovery code:
  27 random symbols (135 bits from the kernel's `getrandom`) and one check
  symbol, in Crockford's base32 alphabet (digits and capitals without I,
  L, O, U). It is shown once as 7 groups of 4, `K7QM-3XRD-…`, to be
  written on paper.
- **Confirmation:** after Enter, the code must be typed back before
  anything is created. A wrong, short or mistyped code shows it again;
  cancelling leaves nothing behind.
- **Check symbol:** Σ (2i + 1) · vᵢ mod 32 over the 27 random values. It
  catches every single wrong symbol and every swap of two neighbours that
  are not 16 apart. A typo is reported as a typo before any scrypt work.
- **Lenient reading:** any case, dashes and spaces anywhere, O read as 0,
  I and L as 1. The canonical 28 symbols are the passphrase.
- **Sealing:** `identity.backup.age` is age scrypt-encrypted to the code,
  at the same log N 18 as every passphrase file
  ([0027](0027-scrypt-fixed.md)). With 135 random bits the work factor
  does not matter for guessing; it is kept the same so one code path and
  one decrypt cap cover every file.
- **`crystal recover NAME DIR`:** checks DIR like `crystal add`, asks for
  the code (echoed), and opens `identity.backup.age` and `vault.age` with
  it, so a wrong code or another vault's backup is refused before any
  passphrase prompt. Then it asks for this device's passphrase:
  - the passphrase given: the vault is re-sealed to it and registered in
    the existing config (a lost `identity.age` is restored);
  - Enter (forgotten too): after the word `replace` is typed, a new
    passphrase, twice, and a new config holding only this vault; the old
    `config.age` is replaced;
  - no config on this device: a new passphrase straight away.

  Nothing is written until the code, the vault, its rollback counter and
  the name are checked. Then `identity.age` is written first, through the
  atomic writer, and the config last; if the config write fails, the
  vault already opens with the new passphrase and `crystal add` can
  register it. The code itself stays valid.
- **No external program and no secret on the command line:** the code is
  typed on the tty only, held in `Zeroizing` buffers, and never appears
  in an error, path or log.

## Alternatives

- **Backup recipients (`age1…`), as before:** need another program or a
  second hardware key. Rejected: Crystal alone must suffice.
- **The backup sealed to the same passphrase:** does not help when the
  passphrase is forgotten, which is the main case.
- **A second passphrase chosen by the user:** people pick weak or
  related ones, and it can be forgotten just like the first; a generated
  code has known strength.
- **A BIP-39 word list:** easy to read, but 2048 words embedded in the
  binary and no lenient matching of near-spellings without more code.
  Base32 groups are short, unambiguous and need only a 32-character
  alphabet.
- **Crystal generating an X25519 key and printing its secret:** an
  `AGE-SECRET-KEY-1…` string is 74 characters with no typo detection,
  much worse to copy by hand.
- **Plain hex:** 34 characters for the same strength and no check.

## Consequences

- Every vault has a recovery path that needs nothing but Crystal and a
  piece of paper.
- Anyone who holds the paper and a copy of the vault can open it. The
  setup guide says to keep it like a house key: off the computer, away
  from the vault's sync and backup locations.
- `identity.backup.age` is now read by `crystal recover`, so a broken
  backup shows up there; the Phase 7 check (`crystal verify-backup`) asks
  for the code instead of a backup key.
- A hardware key or an age recipient as an extra recovery method can come
  later (Phase 8) as an option, never as a requirement.
- `crystal init` costs one more scrypt operation (sealing the backup).
  `recover` costs two to four: opening the backup, opening the config
  when its passphrase is given, sealing `identity.age`, and sealing the
  config when it changed.
- `recover` is a reserved word and cannot be a vault name.
