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

The editor ([decision 0045](../decisions.md#0045)) is modeless, with common Ctrl keys. It edits one note in a zeroizing buffer that it grows by hand and an undo log, both wiped when it closes; only Ctrl+C and Ctrl+X copy into Crystal's clipboard. Ctrl+E saves and shows the editor's text in the pager, the cursor's line on the row it had, and back; Ctrl+Q saves and closes. After `[[` it suggests notes in a list under the cursor, filtered as you type and linked with Enter, Tab or a click; the list holds only note indexes and draws the session's names through the grid ([decision 0045](../decisions.md#0045)). After `[[name#` it lists that note's headings and after `#^` its paragraphs; choosing a paragraph without an id gives it a new `^id`, in another note saved at once through the usual save before the link is written ([decision 0075](../decisions.md#0075)). A click places the cursor, a drag selects, a double click selects a word and a triple click a line, and the wheel moves the cursor; the mouse is captured while the editor or the pager shows, never in the shell or a prompt, so copying out of them takes Shift+drag; in the pager a click follows an internal link or folds a heading's section, and the mouse moving counts as no activity for auto-lock ([decision 0076](../decisions.md#0076)) ([decision 0073](../decisions.md#0073)). A save seals the whole vault to the recipient through the atomic writer; when `vault.age` changed on disk since Crystal read it, the save is refused and nothing is written (checked just before the write, not atomically with it). Unlock keeps only the vault's recipient for saving and wipes the identity; nothing is ever merged.

The editor's code is split so each file does one job and can be audited alone (steps RF1 to RF8); each file's header says its job and the invariants it keeps:

| Module | Its one job | What it keeps |
| --- | --- | --- |
| `editor.rs` | The editor's state; keys and mouse to actions | Terminal-free: keys in, an action out |
| `editor/buffer.rs` | The text and its undo log | Grown by hand, so no copy of note text is left unwiped |
| `editor/motion.rs` | Where the cursor and the selection go | Always on a grapheme boundary |
| `editor/brackets.rs` | `()`, `[]`, `{}` closing as you type | Both brackets go in or neither |
| `editor/suggest.rs` | The link list under `[[`, its keys and mouse | Note indexes and ranges, no copies of names |
| `editor/suggest/query.rs` | The typed link and what it matches | Pure; at most `MAX_MATCHES` entries |
| `editor/suggest/ids.rs` | New `^id`s and the request to save one into another note | Unbiased random ids; a refusal changes nothing |
| `editor/live.rs`, `live/layout.rs` | Live mode's parse: runs, elements, each line's prefix | Ranges of the text only |
| `editor/view.rs` | `View`: state between frames, drawing, scrolling, the mouse | No map of cells: the mouse walks the rows again |
| `editor/view/glyph.rs` | One grapheme: its width and how it is drawn | Decision 0037: nothing hidden reaches the terminal |
| `editor/view/rows.rs` | The row walk, for drawing, scrolling, clicks | One walk for all, so they never disagree |
| `editor/view/look.rs` | Live mode's look of one frame | Whatever the cursor or the selection touches is drawn as written |
| `editor/view/fold.rs` | Folded sections | Kept as offsets, moved by every edit; the cursor never in a fold |
| `editor/view/table.rs` | Boxed tables | A boxed line holds no cursor position |
| `editor/view/suggestions.rs` | Drawing the link list | Names through the grid's checks |

The vault identity is held in a `Box` and wiped on drop by its own key type, not in a `secrecy::SecretBox` as first planned; see [decision 0013](../decisions.md#0013).
