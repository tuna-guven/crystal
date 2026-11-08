# 0028 New passphrases: at least 20 characters

- **Date:** 2026-11-07 (Phase 2 review)
- **Status:** Accepted

## Context

`identity.age` holds the vault's secret key, encrypted to the passphrase,
and it goes wherever the vault is synced or backed up. Anyone with a copy
can guess passphrases offline, at their own pace and on their own
hardware, limited only by scrypt ([0027](0027-scrypt-fixed.md): log N 18,
about 0.5 s per guess on a fast desktop, much less per guess on hardware
built for it). Crystal accepted any non-empty passphrase, so a short one
could be found quickly.

At the backup prompt of `crystal init`, a user could also paste the
backup key's secret half (`AGE-SECRET-KEY-1…`) instead of its public
half. The prompt echoes what is typed, and the line stayed in unwiped
buffers.

## Decision

- **Minimum length:** a new passphrase needs at least 20 characters
  (`crypto::MIN_NEW_PASSPHRASE_CHARS`), counted as Unicode characters,
  not bytes, so a passphrase in a non-Latin script is held to the same
  count. It is checked as soon as the first entry ends, before the
  repeat prompt; a short one ends the command with a message suggesting
  five or more random words, and nothing is created. It applies wherever
  a new passphrase is set: `crystal init` and `crystal add` on a device
  without a config, and the passphrase change of Phase 8. An existing
  passphrase is never checked, so unlock is unaffected.
- **No composition rules or strength meter:** length is the one rule.
  Required digits or symbols push people toward predictable patterns, and
  an estimator such as zxcvbn would be a new dependency for a guess.
- **Backup prompt:** a line that starts with `AGE-` in any case (a secret
  key or a plugin identity) is refused with a note that it is the private
  key and was not stored, and the prompt asks again. The public-line
  prompt now keeps its line and echo in fixed-capacity `Zeroizing`
  buffers that never reallocate, and returns a `Zeroizing<String>`, so a
  pasted secret leaves no unwiped copy. The next prompt clears the
  screen; the alternate screen keeps it out of the scrollback.

## Alternatives

- **12 or 16 characters:** too easy to meet with one word plus a number.
- **Ask again instead of ending:** kinder, but a mismatch already ends
  the command, and both happen before anything is created.
- **Entropy estimate (zxcvbn):** a dependency, and still a guess.

## Consequences

- 20 characters does not make a passphrase strong (a famous quotation is
  long and weak), but it rules out the short ones that offline guessing
  finds first. The setup guide suggests random words.
- A vault made earlier with a shorter passphrase still unlocks wherever
  it is registered, but cannot be added on a device without a config,
  because `crystal add` would set that shorter passphrase as the new one.
  The passphrase change of Phase 8 fixes that.
- Tests use passphrases of 20 characters or more.
