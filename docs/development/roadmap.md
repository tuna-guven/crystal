# Roadmap

## Status (2026-11-14)

| Phase | State |
| --- | --- |
| 0 Skeleton and policy | Done |
| 1 Hardening bootstrap and terminal guard | Done |
| 2 Crypto and vault format | Done (#3 to #15); redesigned in step L1 (decision 0032): fixed vault location, no config file, `rm`, `check-code`, pins |
| 3 Shell and viewer | Done: the Crystal shell with `ls`, `tags`, `find`, `search`, `view`, `links`, `copy` and completion; renderer, sanitizer, pager, Crystal's own clipboard (decisions 0031 to 0041) |
| C Own age code | Done: Crystal encrypts and decrypts with `src/age_v1/`, its own implementation of the age v1 format (decision 0042); the `age` crate is removed, and files it wrote check the code instead (decision 0043) |
| 4 Editor | Done (E0 to E7): the editor (decision 0045), `edit`, `new` (since folded into `edit`, steps N1 to N5), `mv` with link rewriting, `rm`, saving. Both acceptance checks pass: kill -9 during saves, and the strace check in CI (2026-11-14) |
| P Paste report | Done: a changed paste is reported; pastes stay plain text (decision 0047). Steps P0 to P5 below |
| 5 Memory hygiene and comfort | Done (M0 to M6): the zeroizing allocator, `lock`, auto-lock after 5 minutes, lock on suspend, the cow lock screen (decision 0048), the shell starting at the top (decision 0049). Acceptance: the canary scan finds no note text and no key text in memory after lock |
| 6 to 8 | Not started; Phase 7 dropped (decision 0036) |

What runs today: `crystal init NAME` creates a vault in
`~/.local/share/crystal/NAME`, `crystal recover NAME` sets a new passphrase
with the recovery code, `crystal check-code NAME` checks the code,
`crystal rm NAME` deletes a vault, and `crystal unlock NAME` unlocks the vault
into the Crystal shell, which knows `ls`, `tags`, `find`, `search`,
`view`, `links`, `edit`, `mv`, `rm`, `help`, `clear`, `lock` and
`quit`. Crystal locks by itself after 5 minutes without a key, and when
suspended.
`edit` opens the editor (Ctrl+S saves, Ctrl+E shows the note
rendered, Ctrl+Q closes), creating a missing note after asking for its
type (Markdown or text), shown dimmed after every name.

## Steps S0 to S12 (Phase 6, the sandbox)

Approved 2026-11-15, every step; S10 (bubblewrap) stays, not urgent, last. Testing
is local (this machine: kernel 7.3.3, Landlock ABI 11): CI comes later
(S0), and until then CI may fail from S3 on if its runner has no ABI 9.
Security first: Landlock and seccomp go in before anything else in this
phase, then the tests that prove them, then the outer layers. Every
step ends with fmt, clippy, tests, deny and audit green, and leaves
Crystal working.

| Step | What | Why | Size |
| --- | --- | --- | --- |
| S0 (later) | CI: a runner or VM with Landlock ABI 9 or newer, never a weaker mode (sandboxing section 9); first a probe printing the runner's kernel and ABI | From S3 on, Crystal refuses to start below ABI 9, so every pty test fails on an older runner. Deferred by the user: the sandbox is tested locally first | ~10 lines YAML, then the fix |
| S1 (done) | Decision 0059 and the book: the sandbox is entered **before the first passphrase prompt**, not after the identity is decrypted; startup steps 8 to 11 and sandboxing sections 1 and 3 reordered. Code comments naming step 8 as unlock follow in S3, with the code | The lock screen already re-unlocks inside the sandbox (0048), so the first unlock needs no syscall the sandbox lacks; moving it in puts the parsers of `identity.age` and `vault.age` (hostile input, A1) and scrypt inside Landlock and seccomp | docs only |
| S2 (done) | Dependencies: `landlock` 0.4.7 (MIT/Apache-2.0; brings `enumflags2`, reuses `libc` and `thiserror`) and `seccompiler` 0.5.0 (Apache-2.0/BSD-3; no default features, so no serde). Exact pins, `deny.toml` checked | Both already chosen in [Dependencies](../design/dependencies.md); Landlock and BPF by hand would need `unsafe` in the crystal crate | Cargo.toml, lock |
| S3 (done) | `src/harden/landlock.rs`: ABI 9 hard requirement, rules from the vault and state fds already open (vault and state: read, write, truncate, make and remove files; no `REFER`, `EXECUTE`, `MAKE_DIR`, `REMOVE_DIR`, `IOCTL_DEV`), TCP bind and connect handled with no rule, signal and abstract-socket scopes. Called from `harden::enter_sandbox` at the point S1 fixes | The file layer: a bug or a bad crate can no longer read `~/.ssh` or other vaults, nor reach D-Bus or Wayland sockets | ~150 lines |
| S4 (done) | Syscall set collected locally: `src/harden/seccomp.rs` (23 syscalls, by number from `libc` =0.2.191, a direct dependency for its constants only, adding no crate) ran with the log action, uncommitted, through the debug and release suites; the audit log (`run0 ausearch`, run by the user) named `fcntl` and `prlimit64`; a positive control (no `fsync`) proved the log works; 7 runtime syscalls were shown unneeded. `fcntl` in debug builds is std's `debug_assert_fd_is_open`; in release only the resume after a suspend. Committed in kill mode; [Syscalls](../reference/syscalls.md) lists the allowlist | The allowlist comes from what Crystal really calls, not from memory | syscalls.md, ~80 lines |
| S5 (done) | `src/harden/seccomp.rs`: kill-process default and argument rules: `ioctl` only the five terminal requests (so `TIOCSTI` and `TIOCLINUX` kill), `kill` only own pid and `SIGSTOP`, `fcntl` only `F_DUPFD_CLOEXEC` and musl's follow-up `F_SETFD FD_CLOEXEC` (plus std's `F_GETFD` in debug builds), `prlimit64` only reading `RLIMIT_MEMLOCK`, `mmap` without `PROT_EXEC`; `mprotect` not allowed at all. No `openat` flag rule: the filter cannot tell callers apart, and Landlock already limits the paths. Unit test: the filter compiles with 23 syscalls. Mutation-checked: no `TCFLSH` kills every prompt, `kill` with another signal kills the suspend | The syscall layer: no `socket`, `execve`, `ptrace`, `io_uring`, `clone` and no `TIOCSTI`, even if Landlock had a hole | ~150 lines |
| S6a (done) | Debug-only `--sandbox-check-for-test CHECK` (the `*-for-test` naming; release refuses it) and `tests/sandbox.rs`: reading `~/.ssh/id_ed25519`, creating a file outside, and reading another vault are denied with `EACCES`; `/proc/PID/status` shows `NoNewPrivs: 1`, `Seccomp: 2`, one filter; `/proc/PID/mem` unreadable. Mutation-checked: without Landlock the three file checks report `ALLOWED`. Found: std's `File::open` makes musl call the old `open` syscall, which seccomp kills, so the checks use rustix `openat` like the rest of Crystal | Proof that the file layer holds, and the status from outside | ~110 + ~120 lines |
| S6b (done) | Seccomp checks, each dying by `SIGSYS` (exit 159): `socket`, `socketpair`, a thread, `memfd_create`, `mkdirat`, `FIONBIO` (an `ioctl` outside the five), `SIGCONT` to the parent, `exec`; unit test that `TIOCSTI` and `TIOCLINUX` are not in the list. Landlock alone (debug-only `harden::enter_landlock_only_for_test`): TCP connect, a live pathname UNIX socket, `mkdirat`, the parent signal and `exec` refused (`EACCES`, `EPERM`). Mutation-checked per check; found that `socketpair`, threads, `memfd_create` and other ioctls are stopped by seccomp only | Each layer must hold even if the other had a hole (sandboxing section 1) | ~200 + ~60 lines |
| S7 (done) | The refusal without Landlock, end to end: debug-only `--hide-landlock-for-test` (a one-rule seccomp filter: `landlock_create_ruleset` gets `ENOSYS`, as from a kernel without Landlock), test `refuses_to_start_without_landlock` (exit 1, the ABI 9 message, no prompt; positive control reaches the prompt). Mutation-checked: best-effort mode is still refused by the status check; without it Crystal reached the prompt and the test failed | Planned as a unit test of the message; the real path is tested instead, without a VM or `unsafe` | ~30 + ~25 lines |
| S8 (done) | `recover` and `check-code` enter the sandbox right before the recovery code prompt, after the data directory is closed; `check-code` (reads, changes nothing) passes no state directory, so Landlock grants the vault alone (`enter_sandbox` takes `state` as an `Option`). The existing pty tests (`recover_*`, `check_code_*`, including the `restore` path) pass inside it; new test `recover_and_check_code_wait_inside_the_sandbox` reads `Seccomp: 2` from outside, also in release; mutation-checked for each command. `init` and `rm` stay outside | Same protection for the other commands that parse `identity.backup.age` | ~25 + ~20 lines |
| S9 | SELinux module in `packaging/selinux/` (`crystal.te`, `.fc`, `.if`) per section 7, developed permissive then enforcing; `run0` only for `semodule`, `restorecon`, `semanage` and `ausearch`, where it is strictly needed | A layer Crystal cannot switch off itself: it holds even if Crystal's own sandbox code has a bug | ~150 lines policy, manual check |
| S10 (last) | Bubblewrap launcher `packaging/crystal-sandboxed.sh` (sandboxing section 8), checked by hand on this machine, including the bwrap-to-`crystal_t` transition (S9) | An outer layer Crystal does not set up itself: mount and PID namespaces hide the rest of the system even if Landlock had a hole. Not urgent: S3, S5 and S9 come first | ~30 lines script, docs |
| S11 | Fuzzing in a separate `fuzz/` crate: renderer, sanitizer, vault and identity decoders, one hour clean. Needs `libfuzzer-sys` (compiles C++ libFuzzer) and a nightly toolchain, which were removed in Phase 5 | Finds parser bugs property tests miss; the sandbox limits what such a bug can do, fuzzing removes the bug. Weekly CI job only | ~100 lines, new deps |
| S12 | Book and log: sandboxing sections 3, 5, 6, 9 (checkboxes), startup steps 8 to 11, syscalls.md "Pre-sandbox only" cut to what still runs before, threat model A4 and A6, leakage (seccomp kill row), setup guide (kernel requirement), this table; then a security review of the Phase 6 diff before Phase 8 | The book matches the code; the review mirrors Phases 4 and 5 | docs |

Order of work: S1, S2, then S3 to S7 one per turn, then S8, S9, S11,
S12, then S10; S0 when the user asks. Phase 6 acceptance: every check in
[Sandboxing](../design/sandboxing.md) section 9 passes locally; the CI
check follows with S0.

## Steps N1 to N5 (note types shown, not typed; done)

Asked 2026-11-15: a note's type (Markdown or text) is chosen when it is
created and shown dimmed after its name (`Linux Kernel · markdown`),
never as an extension; `mv` cannot change it; `edit` creates a missing
note after asking, and `new` goes. The vault format stays: a title ending
in `.txt` still marks plain text (decision 0038), only hidden.

| Step | What | Why |
| --- | --- | --- |
| N1 (done) | `md::render::shown`: a name without its `.txt`, and its kind; `view`, `edit`, `rm` and `mv` find `todo` stored as `todo.txt` | The shown name must be the one you type |
| N2 (done) | `edit NAME` with no such note asks "Create it?" (y), then the type (m or Enter: Markdown, t: text); `new` removed | User's request: one command opens or creates |
| N3 (done) | `mv` keeps the type: a text note keeps its hidden `.txt`; a typed `.txt` is refused | User's request |
| N4 (done) | `ls`, tag lists, `find`, `search`, `links` and backlinks show `NAME · markdown` or `NAME · text` (the type dimmed); completion offers the shown names | User's request |
| N5 (done) | Decision 0038 brought up to date (no new decision: the change is not security related), 0033 and 0045 command lists, overview, storage, setup guide, roadmap status; all checks | The log states only what is in force |

## Steps A1 to A3 (`crystal unlock NAME`)

Proposed 2026-11-15 together with a centered passphrase box and art beside
the prompt; the user kept only the command (decision 0058).

| Step | What | Why |
| --- | --- | --- |
| A1 (done) | `crystal unlock NAME` opens a vault; a bare `crystal NAME` and an empty command line are refused without echoing the word; `unlock` is not reserved | User's request: every action is a named command |
| A2 (done) | Pty tests start Crystal with `unlock`; `rejects_bare_vault_name` | Tests follow the command |
| A3 (done) | Decision 0058 (cuts 0032), 0023 and 0033, setup guide, backups, sandboxing launcher (`"$bin" unlock "$@" "$name"`); all checks | Book rule in CLAUDE.md |
| W1 (done) | The cow's balloon gets its top border; `crystal unlock` asks under the cow saying "Welcome to Crystal!" | User's request, in place of the dropped art |
| W2 (done) | The welcome and lock screens are centered once when the prompt opens (no redraw on resize; top left when too small); `init`, `recover`, `check-code` unchanged; decision 0058 and setup guide | User's request; about 25 lines, no box |
| C1 (done) | The `Passphrase:` line on the middle row, the cow just above it | User's request |
| C2 (done) | Every passphrase prompt is drawn again on a resize (the resize pipe in `next_keys`), keeping the keys typed; `read_public_line` ignores it | User's request; about 40 lines, no new syscall |
| C3 (done) | Pty test `resize_redraws_the_prompt_and_keeps_the_keys`; decision 0058, setup guide, startup step 6 (left stale by A3), syscalls.md rows | Book rule in CLAUDE.md |
| T1 (done) | Centering and redraw removed again (replaces W2, C1, C2): both cow screens are fixed text at the top left; `read_passphrase_sized` and `size_or_fallback` gone, a resize at a prompt is ignored; pty test `resize_keeps_the_prompt_and_the_keys` | User's request: simpler |
| T2 (done) | Decision 0058 cut in place (no new decision: UI only), setup guide, syscalls.md rows, roadmap; all checks | Book rule in CLAUDE.md |

## Steps L1 to L4 (viewer line-number fixes)

Proposed 2026-11-15, after the user reported three bugs; the user then
asked for the viewer to show no numbers at all.

| Step | What | Why |
| --- | --- | --- |
| L1 (done) | Past the last numbered line j keeps the cursor and scrolls the rows after it into view; k likewise before the first | j went on onto wrapped and empty rows |
| L2 (done) | A table's `\|---\|` line counts as blank (not numbered, not counted); j skips the border under the header | Its number was missing |
| L3 (done) | A code fence (```` ``` ````, ```` ```rust ````, `~~~`), also on a list item's line (`` - ```rust ``), counts as blank | Its number was missing |
| G1, G2 (done) | The viewer's gutter is the bold orange `>` and a blank, without numbers; the editor keeps them | User's request |
| L4 (done) | Decision 0057 (cuts 0053 and 0055), setup guide, dependencies; all checks | Book rule in CLAUDE.md |

## Steps V1 to V5 (viewer and editor line fixes)

Proposed 2026-11-15, after the user reported five bugs. Clicks moving the
cursor (and the orange cursor highlight that went with them) are skipped
for now: mouse capture costs about 250 lines with no security benefit
(0052's reasons still hold).

| Step | What | Why |
| --- | --- | --- |
| V1 (done) | Tables narrower than the screen stay boxed: columns that fit their share keep their width, the rest share what is left; each cell wraps inside its column, a table row is as tall as its tallest cell, shorter cells padded; the row's number on its first screen row only. Unboxed only when a column would get fewer than 3 characters. About 120 lines in `md/render.rs`, tests, escape goldens | A narrowed window broke tables into unboxed text |
| V2 (done) | Line numbers count written lines only: a blank (or whitespace-only) line gets no number and is not counted, so numbers run 1, 2, 3; one counting function used by the viewer and (V4) the editor; `y` still copies the right line. About 15 lines | Numbers jumped (1, 3, 5) over blank lines |
| V3 (done) | j/k and Up/Down move to the next or previous row that has a number (skipping wrapped rows, blank rows and table borders); the screen shows the whole line when it fits. The mouse wheel follows, since the terminal turns it into arrows (no mouse capture). About 30 lines in `pager.rs` | The cursor stopped on rows without a line of their own |
| V4 (done) | The editor gets the same gutter: numbers dim, right-aligned, none on blank lines or wrapped rows, dropped below 20 columns of text; the text area narrows by the gutter. About 50 lines in `editor/view.rs`, tests | The two views show the same numbers |
| V5 (done) | Decision 0055 (partly replaces 0053's numbering and 0045's "no line numbers"; status lines updated), the viewer and editor chapters, setup guide, SUMMARY; fmt, clippy, test, deny, audit, mdbook; one commit | Book rule in CLAUDE.md |

## Steps F1 to F4 (second Phase 5 review; done)

Approved 2026-11-15. The review found that the `restore` question waits
with the vault decrypted and no time limit. The lock balloon's wording
after `lock` was left as it is.

| Step | What | Why |
| --- | --- | --- |
| F1 (done) | The two-clock idle time moves from `app` to `term::Idle` | The prompts need the same clock |
| F2 (done) | `read_public_line` takes a limit; the `restore` question after an unlock passes auto-lock's, and with no key for that long Crystal quits, accepting nothing ([decision 0048](../decisions.md#0048)) | The decrypted vault waited behind the question for good |
| F3 (done) | Pty test `restore_question_times_out` (keys restart the time; the next unlock asks again) | Mutation-checked: no restart, or no limit, fails it |
| F4 (done) | Decision 0048 in place, startup step 11, leakage, syscalls, setup guide, this table | The book matches the code |

## Steps R1 to R7 (Phase 5 review fixes)

Approved 2026-11-14, after a security review of Phase 5.

| Step | What | Why |
| --- | --- | --- |
| R1 (done) | Auto-lock's idle time is the longer of the monotonic and wall clocks; each wait at most 5 s (decision 0050) | The monotonic clock stops while the computer sleeps |
| R2 (done) | `suspend_locks_first_then_stops` checks that the save happened before the stop | It passed when Crystal stopped first and locked after resuming |
| R3 (done) | A test that a key resets the idle timer | No test failed without the reset |
| R4 (done) | Every prompt, the lock screen too, turns bracketed paste off and shows the cursor | A pasted passphrase began with ESC and quit Crystal |
| R5 (done) | The canary scan types its `search` into the shell and fills the completion names (the canary is also the note's title); testing.md says what the scan cannot see (grid cells, the top of the stack, the loop itself) | It overstated what it covers |
| R6 (done) | Docs: the `ioctl` row in Syscalls, `TCFLSH` in the sandbox's ioctl allowlist, the miri line in Testing | They did not match the code |
| R7 (done) | A release-only test that the debug-only flags are refused (`cli::tests::release_refuses_the_test_hooks`) | Checked only by hand |

## Steps M0 to M6 (Phase 5)

Approved 2026-11-14. Each step ends in a commit with passing checks and is
approved before it starts.

| Step | What | Why |
| --- | --- | --- |
| M0 (done) | Decisions 0048 (lock and auto-lock) and 0049 (shell at the top), this plan, the chapters | Fix the behavior before code |
| M1 (done) | The shell starts at the top: the prompt follows the output until the screen is full; `clear` returns it to the top. The setup guide | Decision 0049 |
| M2 (done) | The zeroizing allocator in `crates/zalloc` (zero on free and realloc), installed by `sys`; the only `unsafe` code | Freed copies of note text must not outlive a lock |
| M3 (done) | `lock`: the session ends and everything from the vault is wiped (notes, indexes, recipient, shell output and history, clipboard); the cow lock screen asks for the passphrase again in the same process (a wrong one asks again, Esc or Ctrl+C quits, an older vault asks for `restore`). Unsaved edits are saved first once auto-lock can lock an open editor (M4) | Decision 0048 |
| M4 (done) | Auto-lock after 5 minutes without a key or paste, in the shell, pager and editor | Decision 0048 |
| M5 (done) | `SIGTSTP` locks, then suspends; ignored at the passphrase prompts and questions | A stopped Crystal holds no notes |
| M6 (done) | Acceptance: the canary scan finds no note text in memory after lock; status and book updated | Ends Phase 5 |

## Steps P0 to P5 (the paste report, before Phase 5)

Approved 2026-11-14. Each step ends in a commit with passing checks and is
approved before it starts.

| Step | What | Why |
| --- | --- | --- |
| P0 (done) | Decision 0047 (a changed paste is reported, no question before pasting, no HTML conversion), this plan | Fix what is built, and what is not and why, before code |
| P1 (done) | `shell::keys`: the paste cleaning tells whether it dropped a control or BiDi character or replaced broken bytes; `Key::Paste` carries it as `cleaned` next to `cut` | The report needs the fact from the one place that cleans |
| P2 (done) | Editor: after a cleaned paste, the status line says "Untrusted input sanitized and pasted safely." until the next key; a clean paste shows nothing (counts dropped by the user, 2026-11-14). The setup guide says so | What the user sees |
| P3 (done) | Shell: the same line in its output (not when the paste was cut: the cut is said instead) | The shell takes pastes too |
| P4 | Dropped: ask before a changed paste (decision 0047, Alternatives) | |
| P5 (done) | Check whether Ptyxis removes ESC from pasted text, so a pasted `ESC [ 201 ~` cannot end a paste early; record the result in the threat model. Result: Ptyxis 50.1 (VTE 0.84.1) pastes ESC as `␛`, so it cannot | The one paste risk only the terminal can close |

## Steps E0 to E7 (Phase 4, the editor)

Approved 2026-11-10. Each step ends in a commit with passing checks and is
approved before it starts.

| Step | What | Why |
| --- | --- | --- |
| E0 (done) | Decision 0045 (a simple modeless editor; replaces 0041's automatic copy), this plan, the chapters | Fix the keys and the memory rules before more editor code |
| E1 (done) | `editor::buffer`: zeroizing text buffer grown by hand (copy, wipe the old one), freed bytes overwritten, capped at 8 MiB; grapheme steps; the undo log (200 steps). Pastes keep their line breaks; the shell flattens them | Every later step edits through it; property tests that undo restores the text exactly and grapheme steps land on boundaries |
| E2 (done) | Keys: Shift + arrows, Home and End; pastes up to 8 MiB with a notice when cut (the shell too); Ctrl+Z becomes a key and keyboard suspend goes (`SIGTSTP` still suspends); syscalls.md and the as-built notes | The editor's keys reach it; less special-casing in `term.rs` |
| E3 (done) | `editor::Editor`: the 0045 keys as a terminal-free state machine, like the pager | Tested key by key |
| E4 (done) | The editor's view (soft wrap, tabs, hidden characters spelled out, status line) and Ctrl+E to the viewer and back, previewing unsaved text | Drawing on the grid; property tests that no cell holds a forbidden character and the cursor stays on screen |
| E5 (done) | Saving: the session holds the notes and the recipient (the identity is wiped after unlock); `VERSION` before the first format-2 `vault.age`, generation, counter, indexes; a save over a `vault.age` changed on disk is refused; `edit NAME`, `new NAME`; the acceptance tests (strace, kill -9 during saves) | A usable editor that never writes plaintext and never leaves a broken vault |
| E6 (done) | `mv OLD NEW` rewriting `[[links]]` (keeping `\|shown text` and `#fragment`); `rm NAME`, confirmed with `y`, final | The remaining note commands of 0033 |
| E7 (done) | Phase 4 acceptance, the book finished, status updated | Ends Phase 4 |

## Steps C1 to C3 (done, before Phase 4)

Replace the `age` crate with Crystal's own implementation of the age v1
format ([0042](../decisions.md#0042)), approved
2026-11-09. Each step ends in a commit with passing checks and is approved
before it starts.

| Step | What | Why |
| --- | --- | --- |
| C1 (done) | `src/age_v1/`: base64, Bech32, header and MAC, STREAM, scrypt and X25519 stanzas, `encrypt` and `decrypt`, ported from age 0.12.1; the C2SP/CCTV vectors (`scripts/import-cctv.sh`, `tests/corpus/age-cctv/`); cross-checks with the `age` crate both ways up to 32 MiB; property tests at a million cases | Build and prove the replacement while the old code still runs |
| C2 (done) | `crypto.rs` and `vault/format.rs` switch to `src/age_v1/`; `age` becomes a dev-dependency for the cross-checks; existing vaults and identity files open unchanged; syscalls, architecture and threat-model chapters updated | Crystal encrypts with its own age code |
| C3 (done) | `age` crate removed (`secrecy` already went in C2); before that it wrote `tests/corpus/age-rage/`, which the moved tests now use; integration tests use `check-code`, `recover` and unlock; `deny.toml` has no advisory exception and one duplicate (`syn`); `Cargo.lock` 160 → 78 crates (decision 0043) | No age crate anywhere; the one check lost (age reading a 32 MiB file Crystal wrote) is listed in decision 0043 |

## Earlier steps (Phase 3)

Each step ends in a commit with passing checks and is approved before it
starts.

| Step | What | Why |
| --- | --- | --- |
| D1 (done) | Decisions 0032 to 0036 and the book (docs only) | Fix the design before the code changes, so every step has a written target |
| L1 (done) | Fixed locations ([0032](../decisions.md#0032)), with syscalls.md and the startup chapter's as-built notes updated: `crystal init NAME`, `crystal NAME`, `crystal recover NAME`, `crystal check-code NAME`, `crystal rm NAME`; remove `add`, `remove`, `--at`, DIR, `config.age` and the configured-path walk; `NAME.pin` and the counter in the state directory | Removes about 1,200 lines and the largest untrusted-path surface before anything new is built |
| L2 (done) | Vault format 2 ([0034](../decisions.md#0034)): folders, name rules, no tags field (tags live in the text), reading format 1, the `VERSION` rule | The shell addresses notes by name; the format must be settled before notes can be created |
| L3 (done) | Restore acceptance at unlock ([0036](../decisions.md#0036)) | Makes restores from restic work; small, and it touches the unlock code L1 just simplified |
| L4 (done) | Sanitizer and `SafeText` ([0037](../decisions.md#0037)); the Markdown renderer and plain text notes ([0038](../decisions.md#0038)), with bounded nesting, golden and property tests | `view` needs it; the main defence against hostile note content (A3) |
| L5 (done) | Shell core ([0033](../decisions.md#0033)): prompt line in zeroizing memory, quoting, scrollback and history in memory | The frame every command runs in |
| L6 (done) | L6a: indexes (names, `#tags`, `[[links]]`) and `ls`, `tags`, `find`, `search`. L6b: `view` (the pager: links with `#heading` and `#^id`, backlinks) and `links`. L6c: Crystal's own clipboard (`y`); no system clipboard ([0041](../decisions.md#0041)) | A usable read-only Crystal |
| L7 (done) | Tab completion; the escape-injection corpus with golden files; long property-test runs (`scripts/long-tests.sh`, weekly in CI); invisible characters refused in names ([0040](../decisions.md#0040)) | Ends Phase 3 with its acceptance tests |

## Plan

Phases, each ending in a commit with passing acceptance tests; security layers (hardening, crypto, integrity) come before features, and the sandbox goes in once the syscall set is stable.

1. **Phase 0: Skeleton and policy.** Workspace with the `crystal` crate (modules `harden`, `term`, `cli`, `crypto`, `vault`, `backup`, `model`, `md`, `ui`, `sys`) and the `zalloc` allocator crate. Release profile, musl target, `#![forbid(unsafe_code)]`, `deny.toml`, CI running clippy with warnings denied, tests, `cargo deny check`, `cargo audit`.
   - Accept: `file crystal` reports a static binary; CI is green.
2. **Phase 1: Hardening bootstrap and terminal guard.** Steps 1 to 7 of the startup order; alt screen, raw mode, panic hook, signal handling.
   - Accept: `gdb -p` fails; `VmLck` is non-zero; a forced panic leaves a clean, cleared terminal.
3. **Phase 2: Crypto and vault format.** `crystal init` (including `identity.backup.age`, sealed to a recovery code, and `crystal recover`; decision 0029), unlock, `identity.age`, single vault blob, Padmé padding, atomic writer, generation counters in the state directory. Built with a config file and configurable vault paths; redesigned to fixed locations by decision 0032 (step L1).
   - Accept: round-trip property tests; tamper suite refuses swapped, truncated and bit-flipped vaults; an older vault needs `restore`.
4. **Phase 3: Shell and viewer.** Own screen grid (decision 0031), the Crystal shell (decision 0033), vault format 2 (decision 0034), renderer and sanitizer, the pager with Obsidian links and backlinks, full-text search, Crystal's own clipboard (decision 0041), completion. Steps above.
   - Accept: the escape-injection corpus renders as inert text; the renderer and sanitizer property tests run clean for a million cases each; no note text reaches the terminal's scrollback, stdout or disk. The shell's `lock` command moves to Phase 5, with auto-lock.
5. **Phase 4: Editor** (done; the strace check runs in CI). A simple modeless editor on the grid ([decision 0045](../decisions.md#0045)): arrows, Home/End, PgUp/PgDn, selection with Shift, Ctrl+C/X/V with Crystal's own clipboard (the system clipboard is the terminal's Ctrl+Shift+C/V), Ctrl+Z/Y undo and redo, Ctrl+S save, Ctrl+E to the viewer and back, Ctrl+Q close; about 850 lines. Then `edit`, `new`, `mv` (rewriting links), `rm` (confirmed, final), save (writing `VERSION` before the first format-2 `vault.age`, decision 0034), Syncthing conflicts detected, never merged: a conflict copy is refused at unlock and a save over a changed `vault.age` is refused (decision 0045). Steps E0 to E7 above.
   - Accept: `strace` shows only ciphertext writes inside the vault; kill -9 during save never corrupts the vault.
6. **Phase 5: Memory hygiene and comfort.** Zeroizing allocator, auto-lock after 5 minutes and the shell's `lock` command, lock on suspend (decision 0048), the shell starting at the top (decision 0049). No settings and no vault format change. Steps M0 to M6 above.
   - Accept (passed 2026-11-14): the canary scan finds no plaintext after lock.
7. **Phase 6: Sandbox.** Coverage-guided fuzzing of the renderer, sanitizer and vault decoder in a separate `fuzz/` crate (`cargo-fuzz`; the dependency approved then), run for an hour clean. Landlock and seccomp in log mode, collect syscalls, then switch to kill. SELinux policy module and optional bubblewrap launcher. Debug-only `--sandbox-selftest CHECK` ([Sandboxing](../design/sandboxing.md)).
   - Accept: the checks in [Sandboxing](../design/sandboxing.md) section 9 pass, including that `socket`, `execve` and opening `~/.ssh` are killed or denied.
8. ~~**Phase 7: Backups.**~~ Dropped (decision 0036): backups are left to restic, borg or any file-level tool; Crystal only accepts a restored, older vault after the word `restore` (step L3), and `crystal check-code NAME` checks the recovery code (step L1).
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), optionally a hardware key as a further recovery method (never required; decision 0029), passphrase change, a new recovery code, and key rotation (with an optional stronger scrypt setting, see Storage, Open questions), explicit plaintext export with a warning, reproducible builds, signed releases, embedded dependency list via `cargo auditable`, per-user install to `~/.local/bin` with the optional immutable flag ([Installation](../guide/installation.md)).
   - Accept: two clean builds produce identical hashes; the signature verifies.

A usable daily driver exists after Phase 4; Phases 5 and 6 are what make it meaningfully more secure than gocryptfs plus an editor.
