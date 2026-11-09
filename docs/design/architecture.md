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
| | Terminal guard + shell, pager, editor (own grid and backend)         | |
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
| | Note model + indexes built at unlock (names, links, tags)            | |
| | all notes, folders and titles, in zeroizing memory                   | |
| +----------------------------------------------------------------------+ |
|                                    ^                                     |
|                                    |  plaintext                          |
|                                    v                                     |
| +----------------------------------------------------------------------+ |
| | Crypto (own age v1 code)   <== CRYPTO BOUNDARY                       | |
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
                  | ~/.local/share/crystal/NAME        |
                  | vault.age and identity*.age only   |
                  +------------------------------------+
                                     |  copied, never decrypted
                                     v
                  +------------------------------------+
                  | Your backup tool (restic, borg)    |
                  | ciphertext copies                  |
                  +------------------------------------+
```

The unlock step runs before the sandbox because age plugins are separate binaries; once the vault identity is in memory, exec is denied for the rest of the session.

The vault identity is held in a `Box` and wiped on drop by its own key type, not in a `secrecy::SecretBox` as first planned; see [decision 0013](../decisions/0013-identity-box.md).
