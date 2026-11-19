# Decision log

Every decision that matters for Crystal's security, design or ease of use
gets one paragraph here: cryptography, file formats, sandboxing,
hardening, dependencies, Linux integration, the threat model, the build
and release process. Implementation details do not. Each entry says what
Crystal does today and, briefly, why. Entries are numbered in the order
the decisions were made. A decision that is replaced entirely is deleted;
one that is replaced in part is cut to what still holds and names the
entry that replaced the rest. Git history keeps the old text; numbers are
never reused, so gaps are expected.

To add one: take the next number, add an entry at the end, and update
the design chapter it affects in the same commit.

## 0001 One encrypted vault blob {#0001}

*2026-10-11.* The whole vault (every note's folder, title, timestamps and
body) is one postcard-encoded, Padmé-padded, age-encrypted file,
`vault.age` ([Storage](design/storage.md)). Storage and sync traffic show
neither the note count, nor their sizes, nor which note changed, only the
padded size bucket and the save time, and one age stream authenticates
every note at once, so single notes cannot be swapped, injected or
removed. Per-note files leak exactly that metadata, and an encrypted
filesystem with an external editor leaves plaintext in the page cache and
in swap and temp files. The cost: every save re-encrypts the whole vault
(milliseconds), all notes sit decrypted in locked memory while unlocked,
and a sync conflict is refused, never merged ([0045](#0045)).

## 0003 X25519 vault recipient until rage has hybrid post-quantum {#0003}

*2026-11-06.* The vault's recipient stays X25519, so a copy of today's
ciphertext is not protected against a future quantum computer ("harvest
now, decrypt later"). When rage ships a native hybrid ML-KEM-768 + X25519
recipient, Crystal ports it into its own age code ([0042](#0042)) from the
age specification, with an approved ML-KEM implementation, and re-encrypts
the vault on the next save. Rejected for now: age's `tagpq` (decryption
needs a hardware tag through a plugin, and it pulled in a crate with a
timing advisory), unaudited third-party crates, and plugins (they need
`execve`, which the sandbox denies, [0059](#0059)). Recorded in
[Storage, Open questions](design/storage.md#open-questions).

## 0004 A vault identity, sealed in `identity.age` {#0004}

*2026-11-06.* The vault is encrypted to a random X25519 vault identity,
not to the passphrase: `identity.age` holds the identity, sealed to the
passphrase, so saving needs only the public recipient and never the
passphrase. `crystal passwd` replaces the identity too ([0065](#0065)). The
second copy, `identity.backup.age` under a recovery code, is gone
([0064](#0064)) ([Storage, Key design](design/storage.md#key-design)).

## 0006 Per-user install in ~/.local/bin {#0006}

*2026-11-06.* Crystal installs to `~/.local/bin/crystal` without root,
since secureblue's image is read-only and a system-wide install would need
root for every update. `chattr +i` (root once per update, after restoring
the SELinux label) can make the file immutable. Crystal does not check its
own hash at startup: a replaced binary would skip the check. Same-user
malware (A5) can replace or shadow a mutable binary and ask for the
passphrase ([Installation](guide/installation.md)).

## 0007 Fail closed on inherited file descriptors {#0007}

*2026-11-06.* At startup step 2, before opening anything of its own,
Crystal lists `/proc/self/fd` and refuses to start if any descriptor above
2 is open. Landlock does not restrict descriptors opened before it,
seccomp allows `read` and `write`, and closing a descriptor Crystal does
not own would need `unsafe`. Launchers that leak descriptors must be fixed
or wrapped; CI closes them before `cargo test` rather than weakening the
check ([Startup](design/startup.md)).

## 0009 Signals through self-pipes, no threads {#0009}

*2026-11-06.* Every catchable signal whose default action ends the
process, and `SIGTSTP`, gets a handler (signal-hook's `low_level::pipe`)
that writes one byte to a non-blocking self-pipe; the main loop polls the
pipes with the tty and leaves through the normal exit path, so the
terminal is restored and memory zeroized. signal-hook's iterator and
channel modules need a `socketpair` or a thread, both denied, and
restoring the terminal inside a handler is not async-signal-safe. Crystal
stays single-threaded. Faults, `SIGKILL` and the libc-reserved real-time
signals stay uncaught ([Leakage, Exits that skip
cleanup](design/leakage.md#exits-that-skip-cleanup)); none of them writes
plaintext to disk.

## 0010 Landlock ABI 9 required, no fallback {#0010}

*2026-11-06.* The Landlock ruleset is built with the `landlock` crate in
hard-requirement mode. Below ABI 9 (whose `RESOLVE_UNIX` blocks pathname
UNIX sockets such as D-Bus and Wayland), or without Landlock, Crystal
refuses to start and names the required ABI (the crate does not report the
running one, and asking the kernel directly would need `unsafe`). There is no
best-effort mode, which would run a partial sandbox without saying so, no
override flag, and no raw Landlock syscall (it would need `unsafe`).
Crystal therefore needs a recent kernel, and CI a runner with ABI 9
([Sandboxing, 5.4](design/sandboxing.md#54-fallback-policy)).

## 0011 Atomic writer: same-directory temp, renameat2, crash sweep {#0011}

*2026-11-06.* `vault::atomic_write` is the only way Crystal writes a file.
It removes `.<name>.tmp-<hex>` files left by an earlier crash, creates a
new one with 16 random hex digits in the target's own directory
(`O_EXCL`, `O_NOFOLLOW`, mode 0600), writes and `fsync`s it, `renameat2`s
it over the target (the commit point; a symlink there is replaced, never
followed) and `fsync`s the directory; on an error before the rename the
temp file is unlinked. A crash leaves the old version or the new one,
never a mix, and keeping the temp file in the same directory means
Landlock's `REFER` right is never granted. Names are a validated
`FileName`. Tests inject a failure at every step and `kill -9` a writer in
a loop ([Storage, Save sequence](design/storage.md#save-sequence)).

## 0012 Plaintext exceptions: VERSION, .gen and .pin {#0012}

*2026-11-06.* `atomic_write` takes a `Payload`: `Ciphertext`, which only
the `crypto` module can build, or `PublicBytes`, which only the encoders
of three fixed public formats can build: `VERSION` ([0034](#0034)), the
`*.gen` generation counters ([0019](#0019)) and the `NAME.pin` vault pins
([0032](#0032)). No caller can pass arbitrary bytes, so plaintext on disk
is limited to a format number, generation counts and vault ids, none of
which reveals note content. `VERSION` must be readable before unlock, and
encrypting the local counters would add nothing.

## 0013 Vault identity in a Box, not SecretBox {#0013}

*2026-11-06.* The vault's secret key is held as
`VaultIdentity(Box<x25519::Identity>)`: on the heap, wiped on drop by the
key type itself (x25519-dalek's `zeroize` feature), with no Debug,
Display, Clone or Serialize, checked at compile time. It is serialized
only inside `crypto`, straight into an encryption call, and parsed only
from a decrypted, zeroizing buffer. `secrecy::SecretBox` needs the type to
implement `Zeroize`, and an empty impl to satisfy it would be misleading.
Wiping relies on that dependency feature staying on; the zeroizing
allocator is the backstop.

## 0016 Passphrase prompt without echo or reallocation {#0016}

*2026-11-06.* The passphrase is read from `/dev/tty` in raw mode on the
alternate screen, with no echo, not even asterisks (they show the
length), into one zeroizing 1024-byte buffer allocated once: longer input
is refused, deleted bytes are zeroed at once, and on Enter the buffer
itself becomes the `Passphrase`. Keys typed before the prompt appeared
are discarded (`tcflush`), since the terminal already echoed them. Ctrl+C,
Esc or an exit signal cancels; arrow and function keys are skipped
([0074](#0074)). A new passphrase is
asked twice and compared in constant time. `rpassword` was rejected for
giving less control over buffers and echo.

## 0017 Documentation as an mdBook from docs/ {#0017}

*2026-11-06.* `docs/` is the source of an mdBook (`book.toml` at the
repository root, output in `target/book`): design chapters, user guide,
reference and this log ([0056](#0056)). CI builds it on every push and
publishes it to GitHub Pages from `main`, using only GitHub's own actions
pinned by commit SHA and the official mdBook release binary pinned by
version and SHA-256, since a third-party installer action would add a
party to the supply chain. Every change that makes a decision updates the
book in the same commit. `docs/DESIGN.md` maps the old design document's
sections to chapters, for older issues.

## 0019 Generation counters for rollback detection {#0019}

*2026-11-06.* age authenticates `vault.age`, but an old copy is as
authentic as the newest, so each vault carries a `generation`, +1 per
save, and the never-synced state directory (`~/.local/state/crystal`, mode
0700) keeps the last one seen in `<vault_id hex>.gen` (ASCII decimal and a
newline, one valid spelling, [0012](#0012)). A lower generation is
refused unless the user types `restore` ([0036](#0036)); equal or higher
is accepted and stored; no counter means first use on this device and is
reported. The counter is written after the rename commits, so a crash
leaves it behind the file, never ahead, and a malformed counter is
refused, never reset. Detection is per device, and a process running as
the user (A5) can lower or delete a counter: accepted, since it could log
the passphrase anyway.

## 0022 init: check and ask first, create last, undo on failure {#0022}

*2026-11-06.* `crystal init NAME` checks that the vault directory
([0032](#0032)) does not exist, asks for the passphrase twice before it
creates anything, so a cancelled prompt leaves nothing behind. It then
writes `VERSION`, a new identity and random `vault_id`, `identity.age`
and an empty `vault.age` at generation 1, each through the atomic writer,
checks the result, and writes the pin and the counter. If a step fails,
exactly what this run created is removed and the error says so; a
directory that existed before is never removed.

## 0023 Unlock: one prompt, fail closed, pinned vault id {#0023}

*2026-11-06.* `crystal unlock NAME` ([0058](#0058)) treats every input as untrusted. Before
asking for anything it opens the vault directory ([0032](#0032)), reads
`VERSION`, and refuses while a `vault.sync-conflict-*.age` copy sits
beside `vault.age` (the user keeps one version, [0045](#0045)). After one
passphrase prompt it opens `identity.age`, decrypts `vault.age`
(authenticated as a whole), requires its `vault_id` to equal the pin, so
a vault swapped in under the name is refused, and checks and updates the
generation counter ([0019](#0019)). The passphrase is wiped once the
identity is open, and only the vault's recipient outlives unlock
([0045](#0045)). For startup tests, debug builds accept
`--skip-unlock-for-test`; release builds do not have it.

## 0026 A fixed locked-memory floor {#0026}

*2026-11-07.* Crystal raises the soft `RLIMIT_MEMLOCK` to the hard limit,
refuses to start if that is below a fixed floor (no override; 2 GiB
since [0066](#0066)), then calls
`mlockall(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)`, keeping plaintext out
of swap and hibernation images. Half the floor holds scrypt at the fixed
factor ([0027](#0027)), so every device that starts Crystal can open
every vault. The floor is a fixed number, not derived from RAM, the
limit or speed, because a synced vault must open everywhere. Locking
only secret buffers would miss copies made by libraries. Most
distributions default to 8 MiB, so users raise it once ([Setup, Memory
locking](guide/setup.md#memory-locking-required)).

## 0027 scrypt: one fixed work factor {#0027}

*2026-11-07.* Passphrase files (`identity.age`) are written at one fixed
scrypt factor on every device (log N 20 since [0066](#0066)); nothing
is timed. Decryption accepts up to log N 22 and never more than half
this device's locked-memory limit, and refuses a higher factor before
any scrypt work, so a planted file cannot stall or abort unlock. age's
timed factor was dropped because a fast machine wrote files that smaller
devices refused. Unit tests use log N 10; integration tests run the real
binary at the real factor, with the scrypt crates optimized in debug
builds; no binary can lower the factor. Guessing resistance comes from
this factor plus the 20-character minimum ([0028](#0028)).

## 0028 New passphrases: at least 20 characters {#0028}

*2026-11-07.* `identity.age` travels wherever the vault is synced or
backed up, where it can be guessed offline, so a new passphrase needs at
least 20 Unicode characters (`crypto::MIN_NEW_PASSPHRASE_CHARS`). It is
checked as soon as the first entry ends, in `init` and `passwd`; a short
one ends the command with a hint to use five or more random words, and
nothing is created. An existing passphrase is never checked, so unlock is
unaffected. Length is the only rule: composition rules push people
toward patterns, and a strength estimator (zxcvbn) would be a dependency
for a guess.

## 0031 Own screen grid and terminal backend, no ratatui {#0031}

*2026-11-08.* Crystal already owns the terminal (raw mode, alternate
screen, `ppoll` with the signal pipes, [0009](#0009)), and
`ratatui-core` with `ratatui-widgets` brought 28 crates (proc macros,
`unsafe`, a Zlib licence) for a cell grid, its diff and a few widgets, so
there is no `ratatui` and no `crossterm`. `ui::grid` is one fixed
allocation of cells (a grapheme of at most 16 bytes inline, its width,
its style), wiped on reset and drop, written only through
`Grid::put_str`, with writes outside it clipped, never a panic;
`ui::frame` turns the difference of two grids into cursor moves and
styles on the `/dev/tty` fd. 16 and 256 colors, bold, dim, italic,
underline, reverse and crossed out; no blink, hidden text or RGB. Three
small, exactly pinned crates instead: `unicode-width` (no `cjk` tables),
`unicode-segmentation`, and `pulldown-cmark` without default features (no
HTML writer; Crystal renders and sanitizes every span, [0037](#0037)).

## 0032 Fixed locations, no config file, one `rm` {#0032}

*2026-11-08.* Vaults live only in `~/.local/share/crystal/NAME`
(`XDG_DATA_HOME` is not read), reached from `HOME` one component at a
time with `O_NOFOLLOW`: `.local` and `share` must be the user's and not
group- or world-writable, `crystal` and each vault the user's with mode
0700. There is no encrypted config: it cost about 1,200 lines of path
checks, the largest untrusted input besides the vault format.
`~/.config/crystal` is reserved for a plain-text file Crystal would only
read, not yet created. Each vault has its own passphrase.
`~/.local/state/crystal/NAME.pin` holds the vault's `vault_id`, set at
`init` or the first unlock on a device, so a vault swapped in under
another name is refused. The first word is a command ([0058](#0058)): `crystal init NAME`,
`unlock NAME`, `passwd NAME` ([0065](#0065)), `rm NAME` and `help`;
`init`, `rm`, `recover`,
`check-code` and `help` are reserved and cannot name a vault (the last
two named commands removed with the recovery code, [0064](#0064)). `crystal rm NAME`, after the name is
typed again, deletes the vault for good: Crystal's own files, conflict
copies, temp leftovers, the directory, the pin and the counter; other
files, and then the directory, are left alone. There is no trash.
Deleting is allowed only there and in `init`'s undo, before the sandbox.

## 0033 The Crystal shell {#0033}

*2026-11-08.* `crystal unlock NAME` unlocks and opens a command shell on the
alternate screen, with the prompt `crystal:NAME>` following the output
([0049](#0049)), in place of a multi-pane TUI. No one-shot command prints
note content, so no note text reaches stdout, pipes, logs or the
terminal's scrollback. The output (at most 10 000 entries) and the
history (at most 200) live in zeroizing memory, wiped on quit and lock.
Commands: `ls [FOLDER]`, `ls #TAG`, `tags`, `view`, `search`, `find`,
`links` (broken links), `edit` (creating a missing note after asking),
`mv` (rewriting links to the note), `rm` (asks for `y`; final), `help`, `clear`, `lock`, `quit`. A
line splits on spaces, with double quotes and a backslash before a quote;
no pipes, redirection, globbing, variables or substitution, and nothing
ever runs outside Crystal. The input line is a fixed-size zeroizing
buffer; Tab completes commands and names, Up and Down walk the history.

## 0034 Vault format 2: folders as metadata, links and tags from the text {#0034}

*2026-11-08.* The decrypted vault is postcard records,
`format: u16 = 2 | vault_id: [u8; 32] | generation: u64 | notes`, each
note `id: [u8; 16] | folder | title | created | modified | body`, pinned by
a golden-bytes test. The format number is decoded alone first and an
unknown one refused; the limits (100 000 notes, 8 MiB bodies, a 32 MiB
padded vault, unique ids, no trailing bytes, folders of at most 32 levels
and 1 KiB) and the name rules are checked by decode before any text is
copied and by encode, so Crystal never writes a vault it would refuse.
Folders exist only as prefixes of names (`folder/title`). Names follow
Obsidian: segments of 1 to 255 bytes, no leading or trailing space, not
`.` or `..`, none of `[ ] # ^ | \ / : * ? " < >`, no control, BiDi or
invisible character ([0040](#0040)), unique ignoring case. Links
(`[[name]]`, `|text`, `#Heading`, `#^block`) and `#tags` live in the text;
the indexes are built in zeroizing memory at unlock and after every save,
never stored, and `mv` rewrites links in the same save. `VERSION` (ASCII
decimal and a newline, read before unlock from a regular file) is the
newest format the directory may hold. Format 1 (no folder, a separate
tags field) is still read, its tags appended to the body, and saved as
format 2, `VERSION` first. Rejected: per-note files, a separate metadata
file, a stored link table, CSV and self-describing encodings.

## 0036 Backups with dedicated tools; restoring an older vault {#0036}

*2026-11-08.* Crystal has no backup code: back up `~/.local/share/crystal`
with restic, borg or any file-level tool ([Backups](design/backups.md));
the atomic writer means a backup taken during a save holds a whole old
or new `vault.age`, and one backup plus the passphrase restores
everything. Every save is a fresh ciphertext, so deduplication stores
each version in full; deterministic encryption would show which note
changed. When a vault's generation is lower than this device's counter,
unlock explains that it was restored or put back by someone else, and
only typing `restore` accepts it and lowers the counter.

## 0037 Hidden characters are spelled out, not replaced by U+FFFD {#0037}

*2026-11-08.* `md::safe::sanitize` spells out every control character
(C0 and C1, including ESC and a lone `\r`) and BiDi control as dimmed
`<U+XXXX>`: plain ASCII, visible in every font, showing what was hidden. A
tab becomes four spaces and `\r\n` becomes `\n`. Its output, `SafeText`
(zeroizing, sized once, no Debug, Display, Clone or Serialize), is the
only way note text reaches a widget. The grid and the frame writer draw
`?` for anything still unsafe or unmeasurable (a grapheme wider than two
columns or longer than 16 bytes). U+FFFD is missing from some fonts, and
dropping characters silently would make a Trojan Source note look
harmless.

## 0038 Plain text notes: chosen at creation, stored as a `.txt` title {#0038}

*2026-11-08.* A note is Markdown or plain text, chosen when `edit` creates
it (Markdown by default) and never changed after. In the vault a text
note's title ends in `.txt` (any case): the title is the only marker, so
the vault format is unchanged. The shell never shows the `.txt`; it
shows the type dimmed after the name (`todo · text`), while the viewer's
and editor's title bars show only the name, in bold on the terminal's
background,
`view todo` finds `todo.txt`, `mv` keeps the type (a typed `.txt` is
refused), and no two notes show the same name. Plain text is shown as
written, sanitized and wrapped, with no links or tags read from it, as in
Obsidian; other notes link to it as `[[todo.txt]]`. Other extensions are
not plain text, so titles with a dot do not change kind by accident.

## 0040 Invisible characters are not allowed in note names {#0040}

*2026-11-09.* A title or folder segment may not hold zero-width space,
word joiner, byte-order mark, soft hyphen or Mongolian vowel separator;
they are refused like reserved characters when a vault is read and
written. Two names that differ only by them would look the same in `ls`,
completion and links. The zero-width joiner and non-joiner stay allowed,
since emoji and scripts such as Persian need them. Confusable letters
from different scripts remain possible.

## 0041 Crystal's own clipboard only; copy out with the terminal's selection {#0041}

*2026-11-09.* Crystal's clipboard is zeroizing, locked memory that never
leaves the process: filled by `y` and `Y` in the viewer ([0053](#0053))
and Ctrl+C or Ctrl+X in the editor ([0045](#0045)), pasted with Ctrl+V,
wiped on quit and lock. Crystal never touches the system clipboard: OSC
52 does nothing in VTE terminals and elsewhere hands note text to every
program, and the Wayland or X11 protocol needs a socket through the
sandbox, a library or an external program. To copy out, select with the
mouse and use the terminal's copy key, which copies what is shown, with
hidden characters spelled out. Text from other programs arrives as a
bracketed paste, always text, never keys ([0047](#0047)). Crystal
captures the mouse only in the editor, where copying out takes Shift
while dragging ([0073](#0073)).

## 0042 Crystal's own implementation of the age v1 format {#0042}

*2026-11-09.* `src/age_v1/` implements age v1 itself, ported from age
0.12.1 and age-core 0.12.0 under their MIT licence
(`LICENSES/age-MIT.txt`) and trimmed to X25519 and scrypt recipients over
whole buffers of at most 32 MiB; the parsers, Base64 and Bech32 are small
own code following age's grammar. The `age` crate was 102 of 133 runtime
crates, with build-time advisories, and its 0.12 line pulled in a crate
with a timing advisory. The format is unchanged: files open with any age
implementation, the recovery path of last resort. age remains Crystal's
only encryption format, with no construction or KDF of its own and no
shelling out to `age` or `rage`. Where age and the specification differ,
Crystal follows the specification (legacy stanza encodings and Bech32
with non-zero padding are refused; no grease stanza is written).
Untrusted headers are limited to 64 KiB and 64 stanzas; the file key
lives in a wiped box, derived keys in zeroizing buffers; randomness comes
from `getrandom` (`crypto::fill_random`), which seccomp always allows.
The primitives come from RustCrypto and dalek, exactly pinned
([0051](#0051)). Every applicable C2SP/CCTV vector (92 of 147) passes and
re-encodes byte for byte, with stored files ([0043](#0043)) and property
tests. Plugins and hardware keys need a new decision.

## 0043 Crystal's age code is checked against stored age files, not the age crate {#0043}

*2026-11-09.* The `age` crate is gone, even as a dev-dependency (82 crates
and their advisory exceptions). Before it went, it wrote
`tests/corpus/age-rage/`: X25519 files to one and three keys with age's
grease, at sizes around the 64 KiB chunk edges, and scrypt files as older
Crystal wrote them, stored with their keys, passphrases and plaintext
hashes, and never regenerated. Crystal opens every one and re-encodes its
stanzas, header MAC and payload to age's bytes exactly; the integration
tests use Crystal's own commands on the real binary's files. A one-off
check with Go age v1.3.2 passed both ways ([Testing](development/testing.md))
and is not repeated in CI.

## 0044 The vault's recipient stays private: age does not authenticate the writer {#0044}

*2026-11-09.* age gives integrity under the file key, not authenticity of
the writer: anyone who knows a recipient can write a valid file to it. A
storage or sync attacker (A1) cannot forge a vault only because the
vault's recipient never leaves the device, and a forger would also need
the encrypted `vault_id` to pass the pin. So the recipient is private
data: Crystal never writes, shows, logs or passes it on, and keeps it
only in memory, derived from the identity. Any feature that would expose
it (a second recipient, hardware keys, export, sharing) needs a new
decision on who may then write a vault. No signature of Crystal's own is
added, since that would be a construction outside age ([0042](#0042)).

## 0045 A simple modeless editor with common Ctrl keys {#0045}

*2026-11-10.* `edit` opens Crystal's own editor on its grid,
because an external `$EDITOR` writes temp, swap and undo files in
plaintext. It has one mode and the keys most editors share: arrows,
Home, End, PgUp and PgDn move, with Shift they select, Ctrl+A selects all,
Ctrl+C, Ctrl+X and Ctrl+V copy, cut and paste Crystal's clipboard, Ctrl+Z
and Ctrl+Y undo and redo, Ctrl+S saves, Ctrl+E switches to the rendered
view of the unsaved text and back, and Ctrl+Q closes (twice to discard
changes); any other key does nothing. The brackets `()`, `[]` and `{}`
close as you type: an opening one brings its closer when whitespace, a
closing bracket or the end follows (so `[[` gives `[[]]`) and wraps a
selection, a typed closer steps over the same one, and Backspace in an
empty pair deletes both; quotes are not paired, and pastes never are.
Typing or deleting inside a link's name (after `[[` or `![[` on the
same line, before any `]`, `|`, `#` or `^`) shows the notes whose name
holds every typed word, ignoring case, in a gray list under the cursor:
titles starting with the text first, then by name, the best 100 at most
(so a key costs at most about 45 ms in a vault of 100 000 notes). Up and
Down move its highlight, Enter or Tab (or a click) links that note by its
title, or its full name when another note has the title, adding `]]`;
Esc or any other key closes it. The list holds note indexes and draws
names through the grid, never copying them; it was asked for to match
Obsidian and brings no security benefit (about 410 lines of code,
comments included, and 790 of tests). After a `#` or `^` in the link
the list shows that note's headings or blocks instead, and choosing a
block without an id gives it one ([0075](#0075)); while any list is
open, the key line names what it lists and its keys.
`edit --live NAME`, an experiment asked for to match Obsidian's live
preview (no security benefit; about 400 lines of code, comments
included, and 400 of tests), draws a Markdown note with its markup
hidden and its text styled as in the viewer on every line but those
the cursor and the selection are on, without line numbers. The text, keys, undo and Crystal's
clipboard are the plain editor's, so copying and saving keep the
Markdown as written. Every grapheme drawn is the note's own, through
the same checks as below, or a fixed marker (an icon, `•`, `│`, a
rule); text the parser decodes is never drawn. The whole note is parsed
on every frame (43 ms for 1 MiB in a release build), so a note over
1 MiB is drawn as written. `--live` never creates a note, and a text
note opens as usual.
A Helix-style modal editor was
written in part and removed: far more code on hostile text. Ctrl+Z is a
key, so keyboard suspend is gone (an outside `SIGTSTP` still works,
[0048](#0048)). Undo is a log of edits, at most 200 steps and 64 MiB.
Pastes up to 8 MiB keep line breaks and tabs, and one that does not fit
is refused whole ([0047](#0047)). The text lives in a zeroizing buffer
grown by copying and wiping, never by `realloc`, and is drawn with hidden
characters spelled out ([0037](#0037), [0046](#0046)), with line numbers
([0055](#0055)). Ctrl+S saves the whole vault through the atomic writer
with the vault's recipient, the only secret kept after unlock. If
`vault.age` changed on disk since unlock (Syncthing, a second Crystal),
the save is refused; conflicts are never merged, and the user keeps one
version.

## 0046 Invisible characters inside a visible grapheme are spelled out {#0046}

*2026-11-14.* A grapheme that carries an invisible character (tag
characters, variation selectors, zero-width characters, word joiner,
byte-order mark, combining grapheme joiner, Hangul fillers) is spelled
out whole as `<U+XXXX>`, in the editor and the sanitizer alike, so text
that looks the same is the same and an emoji cannot smuggle hidden data.
Emoji keep what they need: a joiner between two pictographs, and U+FE0F
after a pictograph or in a keycap, judged by a short range table rather
than a Unicode property crate. The flags of England, Scotland and Wales
and ideographic variation sequences are spelled out.

## 0047 A changed paste is reported; pastes stay plain text {#0047}

*2026-11-14.* A bracketed paste is untrusted text (A3), cleaned before use:
control and BiDi characters dropped, broken UTF-8 replaced, line breaks
made `\n`. When the cleaning changed it, the editor's status line or the
shell shows one fixed line, "Untrusted input sanitized and pasted
safely."; a clean paste shows nothing. The paste is inserted at once as
one undo step, with no question first. Crystal never reads the clipboard's
HTML form: that needs a Wayland connection all session (a socket through
the sandbox, and a channel to every other program), or `wl-paste`, a
process Crystal may not spawn. Ptyxis pastes ESC as `␛`, so a paste cannot
forge its own end marker.

## 0048 Lock and auto-lock: 5 minutes, save first, unlock again in place {#0048}

*2026-11-14.* Crystal locks after 5 minutes without a key or paste, fixed
and not configurable, in the shell, the viewer and the editor (counted
as in [0050](#0050)), or at once on `lock` in the shell; there is no lock
key in the editor or viewer. Unsaved edits are saved first; if the save
is refused or fails, Crystal locks anyway and says the edits were lost.
Lock wipes everything from the vault (notes, indexes, recipient,
scrollback, history, clipboard, viewer and editor) and shows the
passphrase prompt under a fixed cowsay cow, with one line on why it
locked. Unlocking runs the same checks as at start, in the same process;
Esc or Ctrl+C there quits. An outside `SIGTSTP` locks first, then
suspends. The `restore` question after an unlock waits with the vault
decrypted, so it has the same limit: with no key for 5 minutes, Crystal
quits and accepts nothing. There are no settings; colors follow the
terminal's theme.

## 0049 The shell starts at the top, like a normal shell {#0049}

*2026-11-14.* The shell's prompt follows its output: a new session shows
the unlock message on the first rows with the prompt below it, each
command pushes it down, and once the screen is full the output scrolls
with the prompt on the bottom row. `clear` returns it to the top. An
empty screen above a bottom-row prompt looked wrong; the alternate screen
keeps it out of the scrollback either way.

## 0050 Auto-lock counts the time the computer sleeps {#0050}

*2026-11-14.* The idle time is the longer of the monotonic clock and the
wall clock, both read at the last input, so the time a laptop sleeps
counts, and setting the wall clock back cannot delay a lock. Each input
wait lasts at most 5 seconds, so Crystal locks within 5 seconds of
waking once the time has passed. `CLOCK_BOOTTIME` or a `timerfd` would
need a rustix feature or new syscalls for the same result. While the
computer sleeps, only the OS's screen lock guards the notes.

## 0051 RustCrypto and dalek primitives moved to their current lines {#0051}

*2026-11-14.* Crystal's age code uses `chacha20poly1305` =0.11.0 (with
its `zeroize` feature, so the AEAD key is still wiped), `scrypt` =0.12.0,
`x25519-dalek` =3.0.0, `hkdf` =0.13.0, `hmac` =0.13.0, `sha2` =0.11.1,
`subtle` =2.6.1 and `zeroize` =1.9.2, exactly pinned, on the lines
RustCrypto and dalek maintain. The move changed no behaviour: the
C2SP/CCTV vectors and the stored age files pass unchanged, byte for byte.
New transitive crates (`hybrid-array`, `ctutils`, `cmov`) were approved.
The next upgrade follows the same rule: exact pins, and the vectors and
stored files decide.

## 0052 The viewer folds sections {#0052}

*2026-11-14.* Headings get an icon and a color per level (`◉ ○ ● ✿ ◆ ◇`),
with their sections indented. A heading's section folds up to the next
peer (marked ` …`) or unfolds again; folds nest, a link or jump inside a
fold opens it, and folds are never saved. n and N select links. How a
section is folded, and how the viewer moves, is in [0076](#0076), which
replaced the keyboard cursor and the viewer without a mouse.

## 0053 The viewer has a gutter; y copies a line {#0053}

*2026-11-15.* A gutter left of the viewer's text holds the headings'
markers ([0057](#0057), [0076](#0076)); the cell colors stay as rendered.
A line break in a note stays a line break, as in Obsidian's reading
view, so every line starts a row. `y` copies a note line as written into
Crystal's clipboard (which one: [0076](#0076)), `Y` the whole note. Numbering rendered rows was rejected: the numbers
would change with the window and match nothing in the editor.

## 0054 Web and mail links are clickable (OSC 8), without their address {#0054}

*2026-11-15.* `[text](target)` and `<target>` links in the viewer become
OSC 8 hyperlinks when `md::link::clickable` accepts the target as the
parser decoded it: `http`, `https` with a host or `mailto` with an
address, only printable ASCII that RFC 3986 allows, valid `%XX` escapes,
at most 2083 bytes. ESC, controls, spaces and BiDi characters therefore
never enter the escape sequence. Anything else is drawn as before, its
address dim after it; nothing is repaired. The target is checked again by
the grid and by the frame writer, which always closes an open link
before the frame ends, a redraw or a prompt. The terminal opens the link;
Crystal spawns nothing and sends nothing. At the user's request a
clickable link's address is hidden, an accepted risk (A3): the text of a
link from an untrusted note can name another site than its target.

## 0055 Lines with text are numbered in the viewer and the editor; j/k go by line; narrow tables stay boxed {#0055}

*2026-11-15.* Only lines with text are numbered, counted from 1, by one
module (`model::lines`) for the viewer, `y` and the editor, which shows
them in a gutter (dim numbers, none on blank or wrapped rows, dropped
below 20 columns and in live mode, [0045](#0045)); which lines count, and the viewer's gutter, are in
[0057](#0057). A table wider
than the screen stays boxed: columns that fit their share keep their
width, the wider ones split the rest, each cell wraps inside its column;
only below 3 columns a cell is it drawn unboxed. How the viewer moves is
in [0076](#0076).

## 0056 One page for the decision log, one paragraph per decision {#0056}

*2026-11-15.* The decision log is this single page, replacing one file per
record with Context, Decision, Alternatives and Consequences. Each
decision is one paragraph under its number, stating only what is in
force. A decision replaced entirely is deleted (0018 into
[0034](#0034), 0024 into [0032](#0032)), a partly replaced one is cut to
what still holds, and the full records stay in git history. The log is
shorter to read and keep current, at the cost of the alternatives and
consequences each record spelled out.

## 0057 The viewer shows no line numbers; fences and `|---|` lines are not counted {#0057}

*2026-11-15.* The viewer's gutter is two columns, a heading's marker
([0076](#0076)) and a blank before the text; it shows no line numbers,
though `y` copies a line by its number. The editor keeps its numbers. Besides blank
lines, a code fence (three or more `` ` `` or `~`, after indentation, `>`
and list markers such as `-` or `1.`; a backtick fence holds no other
backtick) and a table's `|---|` line (only `|`, `-`, `:` and blanks, with
a `|` and a `-`) get no number and are not counted, in the viewer and the
editor alike: the viewer draws no row for them, so their numbers left
gaps. Drawing the fences as dim rows,
and numbering the border under the header, were dropped for this.

## 0058 A vault is opened with `crystal unlock NAME` {#0058}

*2026-11-15.* The first word is always a command: `unlock`, `init`,
`passwd`, `rm` or `help`. A vault is opened with
`crystal unlock NAME`; the bare `crystal NAME` is refused, as is an
empty command line, with the usage, so every action is named and none
is a default. The refusal never repeats the word, which could be a
mistyped passphrase. `unlock` is not reserved as a vault name: the
command always comes first, so `crystal unlock unlock` is unambiguous;
the reserved words of [0032](#0032) stay as they are. The debug-only
test options go with `unlock` only. The passphrase is asked under the
lock screen's cowsay cow ([0048](#0048)), saying "Welcome to Crystal!".
Both cow screens, at unlock and when locked, are fixed text at the top
left: the cow, then the lines below it and the `Passphrase:` line, with
no centering and nothing that depends on the window's size. A resize
while a prompt is open draws nothing again; the keys typed so far are
kept. A passphrase box, a large braille drawing beside the prompt and a
centered, redrawn prompt were dropped: layout code with no security
benefit.

## 0059 The sandbox is entered before the first passphrase prompt {#0059}

*2026-11-15.* `crystal unlock NAME` applies Landlock and seccomp
([Sandboxing](design/sandboxing.md)) as soon as the terminal, the vault
directory (with `VERSION`) and the state directory are open, before the
passphrase is asked. The passphrase, scrypt, the parsing of
`identity.age` and `vault.age` (files a storage attacker can change,
A1), the pin and counter checks, the `restore` question and the whole
session then run confined. The lock screen's unlock ([0048](#0048)) runs
inside the sandbox anyway, so the first unlock needs no syscall the
sandbox would not allow already. `crystal passwd` ([0065](#0065))
enters the same sandbox before its first prompt. `init` and `rm` create and remove
directories, so they stay outside and exit. Nothing Crystal does needs
`execve`. A hardware-key plugin (Phase 8) would; how such a vault
unlocks is decided then, and the sandbox is never weakened for it.

## 0060 Sandbox crates: `landlock` and `seccompiler`, pinned exactly {#0060}

*2026-11-15.* Landlock is applied through `landlock` =0.4.7 and the
seccomp filter is built with `seccompiler` =0.5.0 (no default features,
so no JSON parser), both pinned exactly like the other crates that
guard secrets ([Dependencies](design/dependencies.md)). Writing either by
hand would need raw syscalls and `unsafe` in the crystal crate, which is
forbidden, and BPF and Landlock ABI details are easy to get wrong.
`seccompiler` is pure Rust (no C libseccomp) and needs only `libc`;
`landlock` brings `enumflags2` and its build-time derive macro. Four
crates join `Cargo.lock` (79). `seccompiler` is Apache-2.0 or
BSD-3-Clause and is taken under BSD-3-Clause, already allowed in
`deny.toml`. `libc` =0.2.191, already in the tree through
both, is a direct dependency for its constants only (the syscall and
`ioctl` numbers of the allowlist), so no number is typed by hand.

## 0061 An SELinux domain, `crystal_t`, built from what the code does {#0061}

*2026-11-16.* `packaging/selinux/` holds a policy module: the user's
shell runs `~/.local/bin/crystal` (`crystal_exec_t`) in `crystal_t`,
which may use the terminal, manage `crystal_vault_t`
(`~/.local/share/crystal`) and `crystal_state_t`
(`~/.local/state/crystal`), create those two directories by name, and
only search the directories on the way there. Nothing else: no network,
no exec, no other file, no IPC, no executable memory. It is the layer
Crystal cannot switch off itself, and it confines the process from exec
on, before Landlock and seccomp. Each rule was derived from a full
session in permissive mode and checked against the code; a denial is
answered by changing the code where it can be (`HOME` and the
directories on the way are opened with `O_PATH`, so no `read` is
granted outside Crystal's trees), never by pasting `audit2allow`
output. `filetrans_pattern` is not used, since it also grants reading
and removing in the parent. Installing it needs root once; every install
of the binary is relabeled with `restorecon -F`, as `home_bin_t` is a
customizable type. There is no config type until settings exist
([0032](#0032)). See [Sandboxing, section 7](design/sandboxing.md).

## 0062 Fuzzing in its own workspace, on stable, through a private library {#0062}

*2026-11-16.* Four libFuzzer targets in `fuzz/` (render, sanitize, age,
vault) run Crystal's parsers on hostile bytes: the Markdown renderer and
the sanitizer (A3), Crystal's age code before anything is authenticated,
including round trips and damaged valid files (A1), and the vault
decoder. `fuzz/` is its own workspace with its own `Cargo.lock`
(started from the root one, so shipped crates keep their versions) and
`deny.toml`; `libfuzzer-sys` =0.4.13 and its build-time C++ toolchain
crates never enter Crystal's lock file or binary. The targets build on
the pinned stable toolchain with `--sanitizer none` and debug
assertions: Crystal forbids `unsafe`, so the address sanitizer would
mostly check dependencies, and no nightly toolchain is kept. To give the
targets something to call, the program became a library with a
five-line `src/main.rs`; the library exports only `run`, plus
`crystal::fuzz` under `cfg(fuzzing)`, which only cargo-fuzz sets, so the
modules stay private and dead code is still reported. A fixed X25519
key for the fuzzer exists only under the same `cfg`.

## 0063 An optional bubblewrap launcher, one more layer {#0063}

*2026-11-16.* `packaging/crystal-sandboxed.sh NAME` runs
`crystal unlock NAME` inside bwrap with new user, PID, network, IPC, UTS
and cgroup namespaces, only `HOME` in the environment, and a file system
holding only the binary, that one vault and the state directory; other
vaults do not exist inside it. It is optional and never replaces
Crystal's own Landlock, seccomp and SELinux domain, which apply inside
it as always. It checks Crystal's name rule before building any path and
never creates directories, which would get the wrong label. Since bwrap
runs in `unconfined_bubblewrap_t` and sets no_new_privs, the SELinux
module lets exactly that domain enter `crystal_t` (`nnp_transition`,
`nosuid_transition`); without the rule Crystal would run in bwrap's
domain. See [Sandboxing, section 8](design/sandboxing.md).

## 0064 No recovery code: a forgotten passphrase loses the vault {#0064}

*2026-11-16.* A vault has one secret, its passphrase. `crystal init`
makes no recovery code and no `identity.backup.age`, and `crystal
recover` and `crystal check-code` are gone; the user chose this: "if you
lose your password, you lose your notes, period". A paper code was a
second way into every copy of the vault for whoever found it, and about
1,500 lines of code, prompts and tests. A forgotten passphrase now loses
the notes on every device, and backups do not help, since they open
only with a passphrase that once sealed them; keeping the passphrase in
a password manager is the advice. Older vaults still hold an
`identity.backup.age`: unlock ignores it, `crystal rm` still removes it
as Crystal's own file, and the guide tells the user to delete it by
hand and destroy the paper, since it still opens the vault with the old
code. `recover` and `check-code` stay reserved vault names, so the name
rule does not change. Replaces the recovery code decision (0029) and
cuts [0004](#0004).

## 0065 `crystal passwd` changes the passphrase and the vault key together {#0065}

*2026-11-16.* `crystal passwd NAME` is the only way to change a
passphrase, and it always makes a new vault identity as well: the
passphrase is the key-encryption key, the identity the data-encryption
key, and both change. `identity.age` travels with every sync and
backup, so with a new passphrase alone every older copy would still
open with the old one and give the same identity, which opens every
later `vault.age`; a user cannot know whether such a copy is out there.
Inside the sandbox ([0059](#0059)) it asks for the current passphrase
and the new one twice (at least 20 characters, [0028](#0028)) before
anything is decrypted, so no decrypted vault waits on a prompt; then it
unlocks as `crystal unlock` does (pin, counter, the `restore` question)
and writes three files in an order that leaves a vault opening with the
old or the new passphrase after a crash at any point: `vault.age`
sealed to the old and the new recipient, then `identity.age` with the
new identity under the new passphrase, then `vault.age` to the new
recipient alone, each `vault.age` with the next generation and the
counter after it, and none over a `vault.age` that changed on disk
([0045](#0045)). A failure says which passphrase now opens the vault.
Both recipients stay private ([0044](#0044)); age's several recipients
need no format change, and unlock already opens a vault sealed to two.
A synced device that gets the new `identity.age` before the new
`vault.age` cannot open the vault until the rest arrives. Copies of
`vault.age` from before the change still open with the old passphrase:
nothing can take back a copy someone already has. Rejected: a passphrase
change without a new identity, and a separate `rotate` command (two
commands for one need; the extra cost is one more `vault.age` write).

## 0066 scrypt at log N 20, a 2 GiB locked-memory floor {#0066}

*2026-11-16.* Every passphrase file is written at scrypt log N 20 (1 GiB,
about 2 s on a fast desktop), four times the work per guess of the
earlier log N 18, so a passphrase near the 20-character minimum
([0028](#0028)) resists offline guessing of a copied `identity.age`
longer. It stays one fixed factor for every vault and device
([0027](#0027)), chosen by the user over a per-vault setting, which
would have had to travel with the vault and could be refused on a
smaller device. Half the locked-memory floor must hold it, so the floor
rises from 512 MiB to 2 GiB ([0026](#0026)): every device needs
`RLIMIT_MEMLOCK` of at least 2 GiB, and any process running as the user
may then lock that much. Decryption keeps its absolute cap at log N 22,
not the factor plus 4, so a planted file still cannot ask for more than
4 GiB and about 8 s. Files written at log N 18 still open everywhere;
`crystal passwd` ([0065](#0065)) rewrites them at 20. Integration tests
run at 20 and take about twice as long. Rejected: a per-vault or
per-command setting such as `--scrypt-memory`, and log N 22 or more
(8 s and 4 GiB per unlock, a floor most laptops cannot spare).

## 0067 Reproducible release builds through `packaging/build-release.sh` {#0067}

*2026-11-16.* A release binary is built with `packaging/build-release.sh`:
`cargo build --locked --release` on the pinned toolchain
(`rust-toolchain.toml`, musl, LTO, one codegen unit, stripped), with
rustc's `--remap-path-prefix` mapping the checkout to `/crystal`, Cargo's
registry (under the path Cargo uses and its resolved form) to
`/cargo/registry/src`, and the toolchain's `rust-src`, to which rustc
maps the standard library's `/rustc/<commit>` paths back, to
`/rustc/<commit>` again. Without it the binary held 47 paths naming the
builder's home directory and user name, and its bytes depended on where
it was built. `packaging/check-reproducible.sh` is the Phase 8
acceptance check: two clean clones of `HEAD`, in directories of
different names and lengths and with Cargo's home under two paths,
must give the same SHA-256, and the binary must hold no home path;
checked 2026-11-16, and mutation-checked (without the registry remap the
hashes differ). The flags live in the script, not in `.cargo/config.toml`,
which cannot name `$HOME`; Cargo's `trim-paths` would do the same but is
not stable on 1.99. Builds by other people on other machines need the
same toolchain version and the same `Cargo.lock`; the link uses the
toolchain's own LLD, not the host's linker ([0071](#0071)).

## 0069 Releases are signed with SSH keys on YubiKeys {#0069}

*2026-11-16.* A release is the binary from `packaging/build-release.sh`
([0067](#0067)), its `SHA256SUMS`, and `SHA256SUMS.sig`,
made by `packaging/sign-release.sh` with `ssh-keygen -Y sign`: OpenSSH,
already on every Linux, so neither signer nor user needs another tool.
The keys are two `ed25519-sk` keys, one on each of the user's two
YubiKeys (`crystal-release-1` and `-2`): the private key never leaves
the YubiKey, every signature needs a touch, so malware or a stolen
laptop cannot sign, and the second key keeps releases possible if one
YubiKey is lost. `packaging/allowed_signers` lists both under the
principal `crystal-release`, limited to the namespace `crystal-release`,
so a signature made for anything else (a git commit) is never a release
signature. The script refuses uncommitted changes, so every release is
a commit that rebuilds to the same hash, and verifies its signature
before it ends. Checked 2026-11-16 with both keys: genuine files verify;
a changed `SHA256SUMS`, a changed binary and the wrong namespace are
refused. The keys must also be published outside the repository before
a public release, since whoever controls the repository could replace
`allowed_signers` together with the binary. Rejected: GPG (a second
tool, a keyring and a large attack surface for one signature),
minisign (another program), and an unsigned hash alone.

## 0070 CI runs only what needs no Landlock {#0070}

*2026-11-16.* GitHub's `ubuntu-latest` kernel has no Landlock ABI 9, and
Crystal refuses to start without it ([0010](#0010)); there is no weaker
mode for CI. So the CI workflow runs fmt, clippy, the unit tests
(`cargo test --workspace --lib`, debug and release, with
`RLIMIT_MEMLOCK` at the 2 GiB floor), `cargo deny`, `cargo audit` and the
static release build; every test that starts the binary (`tests/*.rs`)
runs locally before every commit, on a kernel with ABI 9 or newer. A
runner or VM with such a kernel was set aside by the user (roadmap S0).
The cost: the Phase 4 strace check, which needs ptrace capabilities that
secureblue's `deny_ptrace` withholds locally, now runs nowhere; it last
passed on 2026-11-14. The weekly long property tests run the library
(`--lib`; with `--bin crystal`, since the library split of step S11a,
they had run nothing and passed).

## 0071 The toolchain's LLD links; CI rebuilds every release {#0071}

*2026-11-18.* Crystal links with `rust-lld`, the LLD that ships with the
pinned toolchain (`.cargo/config.toml`, `linker = "rust-lld"` for the musl
target), for every build, so tests run the same binary a release ships.
Before, rustc called the host's `cc` and GNU `ld` (ld 2.47 on the
maintainer's Fedora, another version on GitHub's Ubuntu runner), so the
host's linker was an input to the bytes. No dependency compiles C, so with
the linker from the toolchain nothing on the host enters the binary;
the other machine-dependent input, the install path of a build wrapper,
is gone too ([0072](#0072)). A second builder checks this for every
release: `.github/workflows/release-build.yml` builds each `v*` tag (and
on demand) with `packaging/build-release.sh` and prints `release <tag>
<commit> sha256 <hash>`. A release is signed only on the maintainer's
machine, with a YubiKey ([0069](#0069)), and only if both hashes are the
same: `packaging/publish-release.sh` checks the tag, here and on GitHub,
and CI's run, builds, compares, and only then signs and publishes; a compromised build machine or toolchain on either side is then
caught before signing. The workflow keeps a read-only token, signs
nothing, uploads nothing and creates no release: every dependency's build
script runs in it, so it must not hold a key or a token that can change
releases.

## 0072 No embedded dependency list: the build runs no wrapper {#0072}

*2026-11-19.* Release builds are a plain `cargo build --locked --release`
again; `cargo auditable` is gone (it replaced nothing else; decision 0068,
which added it, is deleted). It runs as `RUSTC_WORKSPACE_WRAPPER`, and
Cargo hashes that wrapper's path into the `-C metadata` of every
workspace member, so `crystal` and `zalloc` got other crate hashes, and
the binary another order of constants, wherever the tool was installed:
`/home/user/.cargo/bin` on the maintainer's machine and
`/home/runner/.cargo/bin` on GitHub's runner gave different hashes from
the same commit, while two builds on one machine always agreed. Found by
comparing CI's unstripped build and Cargo's verbose log with a local one
(only those two crates differed) and confirmed locally (a copy of the
tool elsewhere changed the hash). `build-release.sh` unsets
`RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER`, so no wrapper sets the
bytes. A release's dependency list is the `Cargo.lock` of the commit its
notes name, which the reproducible build ties to the binary: `cargo
audit` on that commit checks a release. Lost: `cargo audit bin` and
scanners on a lone binary without its release page. Gained: anyone can
rebuild a release with rustup alone and get the same bytes on any
machine, which is what [0071](#0071)'s second builder checks. Rejected:
`cargo-auditable` at one fixed, root-installed path for every builder
(setup and root for no security gain), and writing the list into the
binary ourselves after linking (new code matching another project's
format).

## 0073 The editor takes the mouse: click, drag, wheel {#0073}

*2026-11-18.* While the editor is on screen, Crystal asks the terminal
for mouse reports (mode 1002, button and drag events, with 1006, SGR
coordinates): a left click puts the cursor on the clicked character, a
drag selects from the press to the button's place, a double click
selects the word (Unicode word boundaries), a triple click the line with
its line break (presses at the same place under 400 ms apart, timed by
Crystal on the monotonic clock, since reports carry no click count), and
the wheel moves the cursor three lines. Any other button, a held modifier and a report
that does not parse are dropped; a report inside a bracketed paste is
pasted text ([0047](#0047)). Reports carry a button and a cell, never
note text, and no note text enters the sequences that turn the mode on
and off. The viewer and the Ctrl+E preview take the mouse their own way
([0076](#0076)); the mode is off everywhere else: the shell, the lock
screen and every prompt,
which also discard pending input, so a late report cannot reach a
passphrase; leaving the screen, a panic and a fatal signal turn it off.
After `kill -9` it stays on in the terminal until `reset`, as bracketed
paste does. Copying out stays the terminal's own selection
([0041](#0041)), with Shift held while dragging in the editor; a plain
click no longer reaches the terminal, so its selection is cleared with a
Shift+click. Accepted
at the user's request: about 340 lines of code, comments included, and
600 of tests, with one more input parser, fuzzed, for no security
benefit. Rejected: scrolling while
dragging past the screen's edge (Shift+arrows extend the selection),
dragging word by word after a double click, Shift+click (VTE keeps it),
and the mouse in the shell or at prompts.

## 0074 Prompts skip escape sequences; only Esc alone cancels {#0074}

*2026-11-19.* At the passphrase prompts (unlock, the lock screen,
`init`, `passwd`) and the public-line prompts (the `restore` question), a
filter in front of the line skips whole escape sequences: `ESC [`, its
parameter and intermediate bytes and a final byte (ECMA-48), and `ESC O`
with a final byte. None of their bytes is typed or kept. Esc alone, at
the end of a read that did not fill the buffer, still cancels, and so do
ESC with any other byte after it (Alt and a key) and a sequence broken by
a control byte. Before, any ESC cancelled ([0016](#0016)), so an arrow
key, and the wheel, which VTE sends as arrow keys on the alternate
screen, quit Crystal at the lock screen (found with the mouse checklist
of [0073](#0073)). The filter keeps only which part of a sequence it is
in; a property test checks that only the typed characters reach the
line, however the input is split into reads. Rejected: turning off the
terminal's wheel-to-arrows mode at prompts (`ESC [ ? 1007 l`), which not
every terminal knows and which would leave arrow keys quitting.

## 0075 Link suggestions list headings and blocks; a chosen block gets an id {#0075}

*2026-11-19.* Inside a link the editor's suggestions ([0045](#0045))
also list what a link can point into, as Obsidian does: after
`[[name#` the headings of the note `name` finds (`Notes::find`; none
when no note or several do), after `[[name#^` its paragraphs and list
items, each shown by its first line; `[[#` and `[[#^` list those of the
note being edited, from the editor's text. Headings holding `[`, `]`,
`|`, `#` or `^` are left out, since they cannot be linked as written;
blocks in code, tables or HTML, and plain-text notes, have none.
Choosing a heading writes `[[name#Heading]]`, a block with an id
`[[name#^id]]`. A block without one gets a new id first: ` ^` and 6
characters from `a-z0-9`, drawn from `crypto::fill_random` (`getrandom`,
already allowed) by rejection sampling, unique in that note (at most 8
tries, else nothing changes), at the end of the block's last line, where
the note index reads ids. In the note being edited the id goes into the
editor's text, in the same undo step as the link, and is saved with it
by Ctrl+S. In another note it is saved at once: Crystal checks that the
block is still there without an id in that note's saved text, puts the
id into a copy of it and saves it through `Session::save` with
`Change::Body`, exactly as Ctrl+S saves a note (the whole vault through
the atomic writer, refused when `vault.age` changed on disk since
unlock), after `md::blocks::add_id` parsed the result again and found
the same blocks; only when that save succeeded (or was made with a
warning, as Ctrl+S counts it) does the editor write the link, and only
while its cursor is still in that link. A refused or failed save writes
no link and reports like a failed Ctrl+S; the demo notes are never saved, so no link is written there.
`Session::save`, the codec and the atomic writer are unchanged: this
is the only place where Crystal changes a note the user did not open,
and only by adding an id the user chose. The list holds note indexes and
byte ranges, never copies of their text, except the headings of the note
being edited: they are scanned from the editor's text into zeroizing
strings, as the note index keeps every saved note's, once per link (an
8 MiB note took 185 ms) and dropped when the list closes. Headings and
paragraphs of other notes are drawn through the grid, as `view` would
show them.
Accepted at the user's request, for no security benefit. Rejected:
vault-wide `[[##` and `[[^^`; rewriting headings with link characters;
keeping the new id unsaved until Ctrl+S (it needs a save of two notes
at once, a change to the save path).

## 0076 The viewer takes the mouse: links by click, heading markers, no cursor {#0076}

*2026-11-19.* While the viewer or the editor's Ctrl+E preview shows,
Crystal asks the terminal for mouse reports with every movement (mode
1003, with 1006, SGR coordinates; the editor keeps 1002, [0073](#0073)).
A left click on an internal link follows it, to its note, heading or
`^block`, as n/N and Enter do, and Backspace goes back; a broken link
says why. A click on a web link only says to Ctrl+click it: opening one
stays the terminal's ([0054](#0054)), since Crystal opening it would
mean starting a process; Ctrl+click opens them in Ptyxis as before (checked by hand). While the mouse is over the row of a heading that opens a
section, a bold orange marker shows in the gutter beside it, `v` for an
open section and `>` for a folded one (plain ASCII, one column in every
terminal); a click on the row folds or unfolds the section, except on a
link in the heading, which it follows ([0052](#0052)).
Markers always shown, and gray, were tried and dropped: too much on the
screen, and too faint to find. The keyboard cursor and its
orange `>` are gone: j/k and the arrows scroll a row, the wheel three,
Space and PgDn/PgUp a screen, g/G to the ends; `y` copies the first
note line shown, Tab folds or unfolds the first heading shown (at or
above the top row); n and N select links and scroll them into view,
Enter follows the selected one. Reports carry a button and a cell, never
note text; one with a modifier, another button, or that does not parse
is dropped, and the mode is off at the shell, every prompt and the lock
screen, and on exit, panic and fatal signal, as in [0073](#0073), every
off sequence also ending mode 1003. The mouse moving with no button held
is not activity: only keys, pastes, clicks and the wheel hold off
auto-lock ([0048](#0048)), or a mouse moved over the window would keep
the vault unlocked; input without activity that arrives past the limit
locks at once, so a stream of motion cannot hold the lock off. Plain drag-selection no longer reaches the terminal
in the viewer; Shift+drag selects, as in the editor ([0041](#0041)).
Accepted at the user's request, for no security benefit: about 250 lines
of code and 350 of tests. Rejected: Crystal opening web links; a double
click to follow; keeping the keyboard cursor beside the mouse.
