# 0023 Unlock: one prompt, fail closed, pinned vault id

- **Date:** 2026-11-06 (#15)
- **Status:** Accepted; the config steps are superseded by
  [0032](0032-fixed-locations.md) (no config, a pin in the state
  directory); the per-note merge of step 4 is dropped by
  [0045](0045-simple-editor.md): the refusal stays

## Context

Startup step 8 turns a vault name and a passphrase into decrypted notes.
Every input is untrusted: the config (anyone holding its recipient can
write one), the path in it, the vault directory and its files, and the
state counters (same-user processes can change them, accepted in
[0019](0019-generation-counters.md)).

## Decision

`unlock::unlock`, after one passphrase prompt:

1. decrypt `config.age` and check `config.gen` (refuse an older config);
2. find the name;
3. walk and check the vault path (replaced by the fixed location of
   [0032](0032-fixed-locations.md)),
   including `VERSION`;
4. refuse a `vault.sync-conflict-*.age` until the per-note merge exists
   (#31), instead of silently ignoring the other side's edits;
5. open `identity.age` with the same passphrase;
6. decrypt `vault.age` (authenticated as a whole by age);
7. require `vault_id` to equal the one pinned in the config, so a vault
   swapped in at the registered path is refused;
8. check and update the vault's generation counter.

Without a `config.age`, Crystal refuses before asking for anything. The
passphrase is wiped once the identity is open; the config fd is closed
before the session. Until the TUI, a placeholder screen shows the note
count only, never a title.

For tests of steps 1 to 7, signals and the terminal guard, debug builds
accept `--skip-unlock-for-test`, which goes straight to the key wait. It
does not exist in release builds, like the other `--*-for-test` hooks.

## Alternatives

- **Ask for the passphrase before checking that a config exists:** asks
  for a secret with nothing to use it on.
- **Decrypt the vault before checking the path:** would read files from a
  directory that might be someone else's.
- **Open a vault despite a sync conflict copy:** a later save would
  overwrite one side's edits.
- **Use a real vault in every startup test:** about four seconds of scrypt
  per test, for tests that never reach unlock.

## Consequences

- Phase 2's acceptance tamper suite (`tests/vault.rs`) runs through the
  real binary: foreign, truncated, bit-flipped and rolled-back vaults and
  configs, a wrong passphrase, a missing `identity.age`, a writable
  parent and a sync conflict copy are all refused.
- A vault with a sync conflict cannot be opened until #31; the user must
  resolve it by hand.
- Debug builds have one more test-only path past unlock; CI and release
  builds must keep it out of release (it is `cfg(debug_assertions)`).
