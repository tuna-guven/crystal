# Architecture

One process, one crypto boundary: everything above the age layer handles plaintext in locked memory, everything below it handles only ciphertext.

```plain
                                        +----------------------+
                                        | Terminal emulator    |
                                        +----------------------+
                                                   ^
                                                   |  keys in, frames out
                                                   v
+-- Crystal process: mlocked, non-dumpable, Landlock + seccomp ------------+
| +----------------------------------------------------------------------+ |
| | Terminal guard + UI (own grid and backend)                           | |
| | alt screen, raw mode, cleared on exit, panic and signals             | |
| +----------------------------------------------------------------------+ |
|             |                                    ^                       |
|             v                                    v                       |
| +--------------------------------+    +--------------------------------+ |
| | Renderer + sanitizer           |    | Editor (own widget)            | |
| | pulldown-cmark events          |    | edits in locked memory         | |
| | control chars stripped         |    | never spawns $EDITOR           | |
| +--------------------------------+    +--------------------------------+ |
|             ^                                    ^                       |
|             |                                    v                       |
| +----------------------------------------------------------------------+ |
| | Note model                                                           | |
| | all notes, titles, links and tags, in zeroizing memory               | |
| +----------------------------------------------------------------------+ |
|                                    ^                                     |
|                                    |  plaintext                          |
|                                    v                                     |
| +----------------------------------------------------------------------+ |
| | Crypto (age crate, rage)   <== CRYPTO BOUNDARY                       | |
| | identity boxed, wiped on drop; decrypts on unlock, encrypts on save  | |
| | unlocked by passphrase (scrypt) or YubiKey plugin, before sandbox    | |
| +----------------------------------------------------------------------+ |
|                                    ^                                     |
|                                    |  ciphertext                         |
|                                    v                                     |
| +----------------------------------------------------------------------+ |
| | Vault I/O                                                            | |
| | single vault blob, Padme padding, atomic rename writes               | |
| +----------------------------------------------------------------------+ |
+--------------------------------------------------------------------------+
                                     ^
                                     |  ciphertext only
                                     v
                  +------------------------------------+
                  | Vault directory                    |
                  | vault.age and identity*.age only   |
                  +------------------------------------+
                                     |  copied, never decrypted
                                     v
                  +------------------------------------+
                  | Backup snapshots                   |
                  | external drive, ciphertext copies  |
                  +------------------------------------+
```

The unlock step runs before the sandbox because age plugins are separate binaries; once the vault identity is in memory, exec is denied for the rest of the session.

The vault identity is held in a `Box` and wiped on drop by its own key type, not in a `secrecy::SecretBox` as first planned; see [decision 0013](../decisions/0013-identity-box.md).
