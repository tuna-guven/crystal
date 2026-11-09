# 0044 The vault's recipient stays private: age does not authenticate the writer

- **Date:** 2026-11-09 (after step C3, before Phase 4)
- **Status:** Accepted.

## Context

age gives confidentiality and integrity under the file key: a changed
byte anywhere in `vault.age` fails at decryption. It does not
authenticate who wrote the file. age is public-key encryption without
signatures, so anyone who knows a recipient (the `age1…` public key) can
write a new, fully valid age file to it. The book said "age proves a
file is authentic", which is more than age promises.

For Crystal this matters against a storage or sync attacker (A1), who
can replace `vault.age`. Today that attacker cannot write a vault that
opens, because the vault's recipient never leaves the device in the
clear: Crystal derives it from the identity in memory, uses it to seal
`vault.age` and to compare keys in `check-code`, and never writes,
shows or logs it. age stanzas do not reveal their recipient, so the
synced files do not give it away either. A forger would also need the
vault's `vault_id`, which the synced files hold only inside the
ciphertext, to pass the pin check.

This was true by accident. Features to come (hardware keys and plugins in
Phase 8, export, sharing, more recipients) could expose the recipient
without anyone noticing what it protects.

## Decision

- **The vault's recipient is private data.** Crystal never writes it to
  a file, shows it, logs it, puts it in an error or passes it to another
  process. It exists only in memory, derived from the identity.
- **The book says what age guarantees:** integrity under the vault's
  key, not authenticity of the writer; a valid `vault.age` comes from
  someone who knows the recipient. Storage (Integrity, Rollback) and the
  threat model (A1, security properties) say so and cite this record.
- **Any feature that would expose the recipient** (a second recipient,
  a hardware-key or plugin recipient, export, sharing a vault) needs a
  new decision that answers who may then write a vault, for example by
  adding sender authentication. Phase 8 must answer this for hardware
  keys.
- The format stays age v1 ([0042](0042-own-age-implementation.md)); no
  signature or MAC of Crystal's own is added now.

## Alternatives

- **Sign or MAC `vault.age` with a key of Crystal's own:** gives writer
  authentication even if the recipient leaks, but is a construction
  outside age, which 0042 rules out while the recipient can stay
  private.
- **Leave it undocumented:** the property would hold until the first
  feature that prints or shares the recipient, and nobody would know it
  was lost.

## Consequences

- A1 cannot forge a vault as long as the recipient stays on the device;
  same-user malware with the vault unlocked (A6) is out of reach of this
  anyway.
- Phase 8 (hardware keys, plugins) and any sharing feature start with
  this question.
- The post-quantum recipient ([0003](0003-no-post-quantum-yet.md)) does
  not change it: hybrid recipients authenticate the writer no more than
  X25519 ones do.
