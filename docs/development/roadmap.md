# Roadmap

## Status (2026-11-19)

| Phase | State |
| --- | --- |
| 0 Skeleton and policy | Done |
| 1 Hardening bootstrap and terminal guard | Done |
| 2 Crypto and vault format | Done (#3 to #15); redesigned in step L1 (decision 0032): fixed vault location, no config file, `rm`, `check-code`, pins |
| 3 Shell and viewer | Done: the Crystal shell with `ls`, `tags`, `find`, `search`, `view`, `links`, `copy` and completion; renderer, sanitizer, pager, Crystal's own clipboard (decisions 0031 to 0041); code on a gray background since steps G1 to G5 |
| C Own age code | Done: Crystal encrypts and decrypts with `src/age_v1/`, its own implementation of the age v1 format (decision 0042); the `age` crate is removed, and files it wrote check the code instead (decision 0043) |
| 4 Editor | Done (E0 to E7): the editor (decision 0045; brackets close as you type since steps B1 to B6, notes suggested after `[[` since steps W1 to W6, their headings and blocks after `#` and `^` since steps J0 to J8), `edit`, `new` (since folded into `edit`, steps N1 to N5), `mv` with link rewriting, `rm`, saving; the mouse since steps X0 to X6 (decisions 0073, 0074), in the viewer too since steps Q0 to Q5 (decision 0076). Both acceptance checks pass: kill -9 during saves, and the strace check in CI (2026-11-14) |
| P Paste report | Done: a changed paste is reported; pastes stay plain text (decision 0047). Steps P0 to P5 below |
| 5 Memory hygiene and comfort | Done (M0 to M6): the zeroizing allocator, `lock`, auto-lock after 5 minutes, lock on suspend, the cow lock screen (decision 0048), the shell starting at the top (decision 0049). Acceptance: the canary scan finds no note text and no key text in memory after lock |
| 6 Sandbox | Done (S1 to S12; decisions 0059 to 0063): Landlock and seccomp entered before the first passphrase prompt, also for `recover` and `check-code`; the section 9 checks as tests, run locally; the SELinux domain `crystal_t`; four fuzz targets, an hour clean; the optional bubblewrap launcher. Skipped by the user: CI (S0) and the `crystal_t` check through the launcher |
| 7 | Dropped (decision 0036) |
| 8 Keys and release | In progress: K0, K1, K4 and R1 to R4 done (decisions 0064 to 0069); both acceptance checks pass (identical hashes, the signature verifies). Next: H0, the hardware-key unlock decision |

What runs today: `crystal init NAME` creates a vault in
`~/.local/share/crystal/NAME` (a passphrase and no recovery code, decision
0064), `crystal passwd NAME` changes the passphrase and the vault key
(decision 0065), `crystal rm NAME` deletes a vault, and `crystal unlock NAME` unlocks the vault
into the Crystal shell, which knows `ls`, `tags`, `find`, `search`,
`view`, `links`, `edit`, `mv`, `rm`, `help`, `clear`, `lock` and
`quit`. Crystal locks by itself after 5 minutes without a key, and when
suspended.
`edit` opens the editor (Ctrl+S saves, Ctrl+E saves and shows the
note rendered, the cursor's line where it was, Ctrl+Q saves and closes), creating a missing note after asking for its
type (Markdown or text), shown dimmed after every name. In the editor the
mouse places the cursor, selects (drag, double and triple click) and
scrolls; after `[[` it suggests notes to link, filtered as you type.
`edit --live` (an experiment) shows the Markdown styled on every line
but the cursor's, as in Obsidian's live preview.

## Steps TB1 to TB6 (live editing: typing inside a boxed table)

Asked for by the user (2026-11-20) after a complexity warning (option B,
the highest-risk one; estimated then at 400 to 600 lines): in live mode
a table stays boxed while the cursor or the selection is in it, as in
Obsidian, and the cursor moves through its cells. Answers given before
this roadmap:

- Tab goes to the next cell, from a row's last cell to the next row's
  first; Shift+Tab to the previous cell, from a row's first cell to the
  row above's last. Tab in the last cell of the last row and Shift+Tab
  in the first cell do nothing: they never add a row or a tab character
  to a table. Outside a table, and in the plain editor, Tab types a tab
  as now and Shift+Tab does nothing.
- Only cell text holds the cursor: the hidden pipes and the padding
  around the text never do. Right at a cell's end goes to the next
  cell's start, Left at its start to the previous cell's end; past the
  table's first or last cell they leave the table as now. Typing `|`
  splits a cell as written Markdown does; the box is redrawn from the
  text on the next frame. As a consequence of that rule, Home and End go
  to the cell's start and end inside a table (the line's start is a
  hidden pipe). The `|---|` line and a short row's missing cells hold no
  cursor position: every move skips them.
- A cell that wraps onto further rows holds the cursor on every one of
  them: each grapheme keeps the row and column it is drawn at, so the
  cursor, the selection and clicks land there.
- Up and Down go to the same column's cell in the row above or below,
  keeping the offset in the cell in graphemes (clamped to its length);
  above the first row or below the last they leave the table as now.
  Not done: moving between a wrapped cell's rows first. The editor moves
  by line everywhere (a wrapped paragraph's rows too), since it does not
  know the screen's width; that would make tables the one place where it
  needs it (about 80 lines more, the view's wrapping given to the
  editor). Up and Down go by table row instead.

How it stays safe and simple: the editor finds the cells itself, on a
key, from the same parse the view draws from (`live::parse`, its
`Layout::tables`), only when live and when the cursor's line holds a
`|`; nothing about tables is kept in step with edits. Every position it
moves to is a cell's text start, end, or a grapheme boundary inside it,
and still goes through `snap`. The element under the cursor inside a
cell is drawn as written, as anywhere in live mode, so its column widens
while it is edited (its written width counts in the column's fit); a box
that no longer fits the screen is drawn as written, as now. A selection
reaching outside the table draws the table as written, as now. No
security benefit; about 450 lines of code and 300 of tests. No new
decision: 0045 is updated in place, as for LT and LW.

| Step | What | Check |
| --- | --- | --- |
| TB1 (done) | The view: a boxed table no longer unboxes for the cursor or a selection inside it; its lines and wrapped rows hold positions (the row walk, `position_at`, `line_row`, scrolling to a cursor on a wrapped cell row); `draw_boxed` draws the cursor and the selection; revealed markup counts at its written width in its column's fit. Found by the property test: a short row's missing cells, which the parser places past the line, hold no position; a table whose cells hold a tab or a character spelled out (drawn wider than measured, so it spilled over its column even before TB1) is drawn as written. About 210 lines | Unit tests: the cursor drawn in each cell, on a wrapped row, at an empty cell; scrolling to it; a click per row; the cursor property test with tables (drawn once, never on a pipe or padding) |
| TB2 (done) | `editor/cells.rs`: the cells around the cursor, pure, from `live::parse` (none unless live and the line holds a `|`): the cell the cursor is in, the next and previous cells, the cell above and below; the editor learns it is live (`set_live`, as `set_note`). About 110 lines | Unit tests: cells of a header row, a body row, a short row, escaped `\|`, a table in a quote and a list, a `|` outside a table, not live |
| TB3 (done) | Left, Right, Home, End in a table, with and without Shift (the selection grows across cells). About 60 lines | Unit tests per key at each cell edge and at the table's edges; selection across cells |
| TB4 | Tab and Shift+Tab: next and previous cell, row to row, nothing at the ends; Tab in the suggestion list still links. About 40 lines | Unit tests: every cell in order and back, the ends, outside a table, the plain editor, an open suggestion list |
| TB5 | Up and Down (with Shift): the same column's cell in the row above or below, the offset kept, leaving the table at its ends; the goal column for leaving is the cell offset. About 60 lines | Unit tests: across rows of different cell lengths, short rows, the `|---|` line skipped, leaving above and below |
| TB6 | A property test: any table, any keys, live: the cursor is never on a hidden pipe or padding of a boxed table, and is drawn once; decision 0045 and the setup guide; fmt, clippy, test, deny, audit, mdbook; the live property tests with `PROPTEST_CASES=5000` | Review of the diff: positions always snapped, no note text in strings, no `unsafe`, syscalls or dependencies |

## Steps RF1 to RF8 (refactor the editor's code before B; done)

Asked for by the user (2026-11-20) before option B (editing inside a
boxed table, as in Obsidian; its own roadmap follows): split the
editor's code so each file does one job, with comments saying why each
block exists, so peers can audit it. No behaviour changes: every test
passes unchanged in every step (tests move with their code), the live
property tests run with `PROPTEST_CASES=5000`, and each moved function
is checked to be the same code (only paths and visibility change). New
code is limited to module headers, why-comments, and two patterns that
make invariants visible in types: a `RowStart` newtype in place of the
bit arithmetic on row starts, and one row iterator in place of the five
hand-written row walks. Later, separately if wanted: `md/render.rs`,
`term.rs`, `session.rs`, `pager.rs`, `commands.rs`.

`src/editor/view.rs` (about 1 800 lines of code and 1 750 of tests)
becomes a directory of single-job modules under `src/editor/view/`:

| Step | What | Check |
| --- | --- | --- |
| RF1 (done) | `view/glyph.rs`: how one grapheme is measured and drawn (shapes, escapes for invisible characters, tab stops, `put_grapheme`) | Tests unchanged; moved code identical |
| RF2 (done) | `view/rows.rs`: the row walk: `RowStart` (a line's row, or a drawn row before a line and its ordinal), `Row`, `row_at`, `row_start`, `top_for`, a `Rows` iterator used by drawing, scrolling, clicks and `line_row` | Tests unchanged; the cursor and scroll property tests with 5000 cases |
| RF3 (done) | `view/look.rs`: live mode's look of one frame (`Live`, renamed `Look`): what is revealed, hidden, skipped; `Look::new` split into named steps (reveal, folds, skipped lines, blank rows, tables), each a function | Tests unchanged; 5000 cases |
| RF4 (done) | `view/fold.rs` (folds kept by offset, moved by changes) and `view/table.rs` (boxed tables: fitting, wrapping, borders, drawing, clicks) | Tests unchanged |
| RF5 (done) | `view/suggestions.rs`: the link suggestion list under the cursor and its mouse mapping; `view.rs` keeps `View` (state, `draw`, scrolling, the mouse) | Tests unchanged |
| RF6 (done) | `editor.rs`: `editor/motion.rs` (cursor moves, words, lines, selection) and `editor/brackets.rs` (closing pairs) apart from the key dispatch | Tests unchanged |
| RF7 (done) | `editor/suggest.rs`: `suggest/query.rs` (what is typed after `[[`, pure matching), `suggest/ids.rs` (new `^id`s and the block request), the list and its keys staying in `suggest.rs` | Tests unchanged |
| RF8 (done) | The architecture chapter's module map; every module's header says its one job and its invariants; fmt, clippy, test, deny, audit, mdbook | Review: no logic changed (diff of moved bodies), no new `unsafe`, syscalls or dependencies |

## Steps LW1 to LW4 (live editing: wide tables as in the viewer; done)

Asked for by the user (2026-11-20) after a complexity warning (option B
of three; the viewer's unboxed fallback and leaving it as written were
declined): a table too wide for the screen looks as in the viewer while
the cursor is not in it, its columns narrowed by the viewer's
`fit_columns` and each cell wrapped inside its column by the viewer's
`wrap_each`, a row as tall as its tallest cell. Those extra rows show
text from several cells side by side, so they cannot be ranges of the
note like other rows. They are drawn rows like the border rows: no
position, since the cursor is never in a boxed table (it unboxes the
table). Every boxed row is drawn whole from its cells (LT3's per-byte
cell borders go), so fitting and wide tables take one path. A table
whose columns would keep fewer than 3 columns stays as written (the
viewer unboxes it). No security benefit; estimated at about 200 lines
of code net and 150 of tests, done in about 160 net (LT3's per-byte
cells removed) and 65 of tests.

| Step | What | Check |
| --- | --- | --- |
| LW1 (done) | Drawn rows by list: the 2-bit row marks become one ordered list of drawn rows before each line (a blank row, a border, later a table row's continuation), a row start's top bit and an ordinal saying which. A refactor: rows drawn as now. About 40 lines changed | All tests unchanged; the cursor property test with `PROPTEST_CASES=5000` |
| LW2 (done) | Table rows drawn whole: a boxed line is one row, not wrapped, drawn from its cells' ranges, padded by alignment (its `|---|` line as `├─┼─┤`); LT3's `Show::Cell`, hidden runs, prefixes and suffixes go. A click lands at the clicked cell's text. Same look as now | LT tests unchanged but the click; a click per column |
| LW3 (done) | Wide tables: column widths by `fit_columns` (made public in `md::render`), cells wrapped by `wrap_each` over their drawn text, trailing spaces dropped; continuation rows before the next line, ahead of its border | Unit tests: a wide table's rows, a tall cell in the last row, scrolling over continuation rows; the cursor property test |
| LW4 (done) | The comparison with the viewer for a wide table is LW3's `live_wide_tables_wrap_cells_as_the_viewer` (at a real terminal's widths; `live_rows_are_the_viewers` draws live mode a column wider to line up paragraphs, so a table fits differently there); decision 0045 and the setup guide; fmt, clippy, test, deny, audit, mdbook; live property tests with `PROPTEST_CASES=5000` | Review of the diff: row starts, note text, `unsafe`, syscalls, dependencies |

## Steps LT1 to LT4 (live editing: boxed tables; done)

Asked for by the user (2026-11-20) after a complexity warning: a table
the cursor and the selection are not in looks as in the viewer: a
`┌─┬─┐` row above it, `│` cells padded and aligned, `├─┼─┤` for the
`|---|` line and, as the viewer draws it, between every two body rows,
a `└─┴─┘` row below it; cells styled (bold, links, `code`) with their
markup hidden. With the cursor or the selection in it, the table is
drawn as written (indented, cells still styled, the element under the
cursor as written), so column widths never move while typing. A table
whose box does not fit the screen is drawn as written too (the viewer
wraps its cells instead). The border rows are rows the note does not
have: the viewer's added blank row (`GAP`, step LV2) becomes one of
three row marks before a line, in this order: a table's bottom border,
the blank row, a table's top or inner border. No security benefit;
estimated at about 250 lines of code and 180 of tests, done in about
370 and 255. Rules as for LV1 to LV3.

| Step | What | Check |
| --- | --- | --- |
| LT1 (done) | Parsing: a table is no longer drawn raw (cells styled, their markup elements the cursor reveals); the layout records each table: its lines, alignments, each row's line and cells (trimmed), and each column's width as drawn (from the runs). About 70 lines | Unit tests: cell ranges and widths with hidden markup, a table in a quote and in a list, short rows; the existing property tests |
| LT2 (done) | Row marks: `GAP` becomes a 2-bit mark (bottom border, blank row, top or inner border); every reader of a row start strips the mark; a table not edited and fitting the screen is boxed, its border rows drawn from fixed pieces (`QUOTE_BAR` dashes), skipped in folds; a click on one goes to the line below. About 100 lines | Unit tests: rows of a boxed table at the text's start and end, after a heading, before a blank line; scrolling over border rows; the cursor entering unboxes |
| LT3 (done) | Boxed lines: the prefix gains `│ ` and the first cell's padding, the bytes between two cells draw as ` │ ` with both paddings, the rest of the line as padding and ` │`, the `|---|` line as `├─┼─┤`, all fixed strings; a click on padding lands on the cell's nearest character. About 80 lines | Unit tests on the drawn grid: alignment, wide characters, styled cells, a short row; the cursor property test with tables |
| LT4 (done) | The comparison test gains tables (`live_rows_are_the_viewers`); decision 0045 and the setup guide; fmt, clippy, test, deny, audit, mdbook; the live property tests once with `PROPTEST_CASES=5000` | Review of the diff: every row start reader strips the mark; no note text in strings, no `unsafe`, syscalls or dependencies |

## Steps LV1 to LV3 (live editing: the viewer's rows; done)

Asked for by the user (2026-11-20) after comparing screenshots, with a
complexity warning: live mode's rows match the viewer's while not
edited. The viewer gives no row to a code block's fences or a Setext
underline, merges blank lines into one between two blocks, drops them
before the first block and after the last, and adds one between two
blocks the note does not separate. No security benefit; about 210 lines
of code and 150 of tests.

| Step | What | Check |
| --- | --- | --- |
| LV1 (done) | Lines without a row: the layout marks fences and Setext underlines; the view skips them and extra blank lines like folded ones, unless the cursor or the selection is on them or a revealed element covers them (the cursor in a code block shows its fences). About 60 lines | Unit tests: each kind skipped, shown again under the cursor; the cursor property test with fences, blank runs and underlines |
| LV2 (done) | The viewer's added blank row: a row with no text before a block the note does not separate from the one above (a heading, a paragraph after a list, a list after a paragraph), counted by the row walk, scrolling and the mouse (a click there goes to the line below). About 150 lines | Unit tests on rows, scrolling and clicks; the property test |
| LV3 (done) | The comparison test checks rows as well as columns (`live_rows_are_the_viewers`, a note shaped like a real one; checked once that it fails without LV1 or LV2); decision 0045 and the setup guide; fmt, clippy, test, deny, audit, mdbook | Review of the diff: every reader of a row start strips or checks the gap mark; no note text in strings, no `unsafe`, syscalls or dependencies |

## Steps LF1 to LF4 (live editing: folding and links as in the viewer; done)

Asked for by the user (2026-11-20) after a complexity warning: in live
mode, whatever is not being edited works as in the viewer. Headings
fold: the viewer's marker shows in the gutter under the mouse (bold
orange `v` open, `>` folded), a click on it folds or unfolds the
section, a folded heading ends in ` …`. A click on an internal link
(not drawn as written) follows it: its note opens live, at its heading
or block; with unsaved changes Crystal says to save first, as Ctrl+E
does. Web links are the terminal's (Ctrl+click), their address hidden
(decision 0054). What cannot work as in the viewer, since keys type
text in the editor: Tab, n/N, Enter, Backspace (back), y and Y; and a
click on a heading's text places the cursor, so only the marker folds.
A fold opens when the cursor or the selection enters its section (Up,
Down, a paste, undo), so the cursor is never hidden. No security
benefit; about 330 lines of code and 300 of tests. Rules as for LE1 to
LE7; motion reports in the editor follow decision 0076's rule (they
never count as activity).

| Step | What | Check |
| --- | --- | --- |
| LF1 (done) | Folds: the buffer records each change's offsets (never text) for the view; the layout lists headings outside quotes and lists; the view keeps folded headings by offset, moved with each change, dropped when their heading goes, opened when the cursor or selection enters; the row walk skips a folded section, its heading ends in a dimmed ` …`. About 120 lines | Unit tests: fold and unfold, edits above and inside move or drop the fold, the cursor entering opens it, scrolling past a fold; the cursor property test with folds |
| LF2 (done) | Markers: motion reports on in the live editor (buttons only in the plain one, as now), the marker on the hovered heading's gutter cell, a click on it folds or unfolds. About 70 lines | Unit tests: hover, click, clicks elsewhere place the cursor as before |
| LF3 (done) | Links: `live::parse` records each link's element and whether it is internal; a click on a link not drawn as written follows it (internal: its note live at its heading or block, unsaved changes: save first; web: Ctrl+click it); web links carry the terminal's hyperlink as in the viewer. About 140 lines | Unit tests: which clicks follow; a binary test: a click follows a `[[note#heading]]`, motion reports off after leaving the editor |
| LF4 (done) | Decisions 0045 and 0076 updated in place (motion reports in the live editor), setup guide, `scripts/check-mouse.sh` items, fmt, clippy, test, deny, audit, mdbook | Review of the diff for mode sequences left on, the activity rule, note text in strings, `unsafe`, syscalls, dependencies |

## Steps LR1 and LI1 to LI3 (live editing: reveal by word, viewer indentation; done)

Asked for by the user (2026-11-19) after trying `edit --live`, with a
complexity warning: (LR) only the element the cursor is in or beside is
drawn as written (`**bold text**`, a link, inline code; a heading while
the cursor is on its line, a code block's fences while it is inside,
a list marker or a quote's `>` when the cursor touches it); a space
typed after `**bold text**` styles it again; a selection reveals every
element it touches. (LI, "full match") lines are indented as in the
viewer: 2 columns per heading level for a section, `│ ` per quote,
list items at the viewer's widths with wrapped rows hanging under
their text, code blocks as a gray box padded to their widest line;
the line's own leading markup (spaces, `>`, `-`, `#`) is hidden while
the viewer's indent stands in for it. What cannot match, since the
editor shows the note's lines: blank lines stay as written (the viewer
adds and merges blank rows), a code block's fence lines stay as gray
rows at the box's top and bottom, tables stay as written (indented),
and rows wrap by character, not by word. No security benefit; about
330 lines of code and 250 of tests (with LE1 to LE7: about 1 000
and 700). Same rules as LE1 to LE7.

| Step | What | Check |
| --- | --- | --- |
| LR1 (done) | Reveal by word: `live::parse` also returns each element's range (inline tags, inline code, headings, fenced code blocks, list markers, quote `>`s, rules); the view draws as written only the elements the cursor touches (start to end, both ends included) or the selection overlaps, in place of whole lines. About 60 lines | Unit tests: typing a space after `**b**` styles it; the cursor on a heading line shows `#`; a selection reveals what it touches; a property test that every hidden or replaced byte lies in an element (so the cursor is never on something hidden) |
| LI1 (done) | Line layout in `live.rs` (`live/layout.rs`): for each line, its leading markup, the viewer's prefix for its first row and for wrapped rows (section indent, quote bars, list markers or their width, heading icons), and a code line's gray padding, from the same container rules as `md::render`. Not drawn yet. About 150 lines | Unit tests: prefixes for sections, nested lists, quotes in lists, headings in quotes, code in a list |
| LI2 (done) | The view draws the layout: a row's width less its prefix, prefix and padding drawn, leading markup hidden unless revealed (then the section indent and the markup as written), a click on a prefix to the row's start. About 120 lines | Unit tests on the drawn grid; the cursor property test with nested containers |
| LI3 (done) | Same as the viewer: a test renders sample notes in both and compares each text line's start column; decision 0045 and the setup guide updated; fmt, clippy, test, deny, audit, mdbook | The comparison test; review of the diff (format strings, unchecked graphemes, `unsafe`, syscalls, dependencies: expected none) |

## Steps LE1 to LE7 (live editing, experimental; done)

Asked for by the user (2026-11-19) after a complexity warning, as an
experiment: `edit --live NAME` opens a Markdown note in the editor with
its Markdown shown as in Obsidian's live preview. Every line the cursor
or the selection is on is drawn as written, exactly as today; every
other line has its markup hidden and is styled as in the viewer (`# `
gone and the heading colored, `**`, `*`, `~~` and backticks gone, a
`[[link|alias]]` or `[text](url)` shown as its words, list markers as
`•`, dimmed task boxes, quote bars, code blocks gray with their fences
hidden). Tables stay as written. `edit NAME` is unchanged; a text note
opens as usual. No security benefit; about 600 lines of code and 400 of
tests.

How: the buffer, the keys and the undo log stay as they are, so
Crystal's clipboard (Ctrl+C, Ctrl+X, Ctrl+V, Ctrl+A) copies and pastes
the Markdown as written, in both modes. Only drawing changes: a new
module `editor/live.rs` parses the editor's text with pulldown-cmark
(already a dependency) and its source offsets into runs of the text's
own bytes, each shown, hidden or replaced by a fixed string (`•`, `☐`,
`│`), with a style. The view's row walk takes those runs: a hidden byte
is zero columns wide. Every grapheme drawn is still a grapheme of the
note passed through the editor's own check (`shape`, decision 0037),
or a fixed string; nothing the parser decodes (entities, escapes) is
drawn. Up and Down stay by note line, as now; the mouse finds a
grapheme by the same row walk, so a click on a styled line lands on
the character under it. Shift+drag in the terminal copies what is
shown, as in the viewer.

Rules for every step: no new dependency, no new syscall, no change to
Landlock, seccomp or hardening; no note text in an error, a log, a
panic or a format string. Not security related, so no new decision:
decision 0045 is updated in place in LE7. Every step ends with fmt,
clippy, tests, deny and audit green.

| Step | What | Check |
| --- | --- | --- |
| LE1 (done) | `edit --live NAME`: `Open::Edit` carries `live`; the editor's `View` keeps it (`View::live`); a missing note is not created with `--live` (says to create it with `edit NAME` first); a text note opens as usual; the title shows `live` after the name (`live [+]` when changed). Nothing else is drawn differently yet. About 40 lines | Unit tests: `edit --live roadmap` opens note 0 live, `edit roadmap` not live, `edit --live` alone asks which note, `edit --live missing` refused |
| LE2 (done) | `editor/live.rs`: inline markup. `runs(text) -> Vec<Run>` from pulldown-cmark's offset iterator: emphasis, strong, strikethrough and inline code markers hidden, their text styled; links and `[[links]]` shown as their words; headings' `#` hidden, the line colored per level as in the viewer. Not used by the view yet. About 150 lines | Unit tests per element; a property test: runs cover the text in order without overlap, every range on char boundaries |
| LE3 (done) | The view draws live: row walk and drawing take the runs, a hidden byte is zero wide; lines touched by the cursor or the selection are drawn as written. About 120 lines | Unit tests on the drawn grid: a styled line, the cursor's line raw, wrapping by shown width; a property test: any text, any cursor, any width: no panic, the cursor drawn |
| LE4 (done) | Blocks: list markers as `•`, task boxes dimmed `[ ]`/`[x]` as in the viewer, quote bars `│`, code blocks gray with fences hidden, rules `─`. About 100 lines | Unit tests per block |
| LE5 (done) | Mouse and clipboard: clicks and drags on styled lines land on the grapheme under them; Ctrl+C copies the Markdown as written, Ctrl+V pastes it unchanged. Measured in a release build: a live frame costs 4.6 ms for 0.1 MiB, 43 ms for 1 MiB, 370 ms for 8 MiB (as written: 0.2, 1.1, 8 ms), and a cache would help only moving, not typing, so instead of one (the user's choice) a note over 1 MiB (`LIVE_MAX`) is drawn as written, `live` leaves the title, and opening one says so. About 15 lines | Unit tests: a click on a styled word or a heading's icon; copy and paste in live mode; the size limit |
| LE6 (done) | Through the binary: `edit --live` shows a styled note (title `live`, a line away from the cursor without its markup), Ctrl+S saves the Markdown as written (the plain editor shows it again), `edit --live` of a missing note refuses | The binary test `live_edit_styles_and_saves_the_markdown`; checked once that it fails when `--live` opens the plain view |
| LE7 (done) | Acceptance: decision 0045 updated in place, setup guide (`edit --live`), help text, status line; fmt, clippy, test, deny, audit, mdbook | Review of the LE1 to LE6 diff for note text in format strings, unchecked graphemes drawn, `unsafe`, syscalls or dependencies (expected: none) |

## Steps Q0 to Q5 (the mouse in the viewer; done)

Asked for by the user (2026-11-19) after a complexity warning: in the
viewer (and the editor's Ctrl+E preview) a single click on an internal
link follows it, to its note, heading or `^block`; every heading line
gets a gray `>` in the gutter, bold while the mouse is over it, and a
click on it folds or unfolds the section. The keyboard cursor and its
orange `>` are dropped (the user's choice "b"): j/k, the arrows and the
wheel scroll, `y` copies the first note line shown, Tab folds the first
heading shown. Web links stay the terminal's (Ctrl+click, or
Shift+Ctrl+click if Ptyxis gives Crystal the Ctrl+click while it takes
mouse reports): Crystal never opens a URL, since that would mean
starting a process. No security benefit; about 250 lines of code and
350 of tests.

Rules for every step: no new dependency, no new syscall, no change to
Landlock, seccomp or hardening; no note text in a mouse mode sequence, an
error, a log or a panic; mouse modes are off at every prompt, the lock
screen, the shell, on exit, panic and fatal signal, as in decision 0073.
Motion reports (the mouse moving with no button held) never count as
activity: only keys, pastes, clicks and the wheel hold off auto-lock
(decision 0048). Every step is one commit, ends with fmt, clippy,
tests, deny and audit green, then the security check in its row and a
review of that step's diff.

| Step | What | Security check before the commit |
| --- | --- | --- |
| Q0 (done) | Decision 0076 (security related: mouse reports in more screens, motion reports, the auto-lock rule): the viewer's mouse as above; 0073's "no mouse outside the editor" cut to what still holds, 0052 and 0053/0057 (the cursor and its `>`) cut to what still holds; leakage chapter unchanged (reports carry cells, never text). Docs only | The decision states the mode on/off places and the activity rule |
| Q1 (done) | Input and modes: the decoder reads SGR button 35 (moved, no button) as `MouseEvent::Move`; `Screen` takes a mode (off, buttons 1002 for the editor, motion 1003 for the viewer, each with 1006), and every off sequence (`MOUSE_OFF`, the full redraw, the terminal guard's exit and panic paths) turns 1003 off too; `app.rs` resets the auto-lock clock only when a read decodes to something other than `Move`; the editor ignores `Move`. Nothing turns 1003 on yet. About 50 lines | Unit tests: decoding (35 is `Move`, modifiers still drop); every off sequence holds `?1003l`; a read of only motion reports leaves the idle time running, one with a key resets it. The `keys` fuzz target builds (runs skipped) |
| Q2 (done) | The cursor goes: j/k and the arrows scroll a row, Space and PgDn/PgUp a screen, g/G to the ends; `y` copies the first note line shown (a notice when none), Tab folds or unfolds the first heading shown at or above the top; n/N select links and scroll them into view, Enter follows the selected one; the orange `>` and its style are removed; key line and help text. About 60 lines changed | The pager's tests rewritten for scrolling; a property test that any keys keep the scroll in bounds and a selected link shown |
| Q3 (done) | Heading markers: a gray `>` in the gutter on each section's heading line, drawn bold when hovered; `Pager::mouse` (a `Move` over a marker sets the hover, elsewhere clears it; a press on a marker toggles that section's fold; the wheel scrolls 3 rows); the pager records, while drawing, which line each row shows. About 60 lines | Unit tests: hover bold, fold and unfold by click, presses outside do nothing; a property test with any mouse events: scroll in bounds, hover only on a heading row, no panic |
| Q4 (done) | Links by click: while drawing, the pager records each link's cells (`Page::draw_line` reports runs with a link); a press on an internal link follows it as Enter does (note, heading, `^block`, Backspace back), a broken one says why, a web link says to Ctrl+click it; mode 1003 on in the viewer and the preview, off everywhere else. About 60 lines | Through the binary: a click on a `[[note#heading]]` shows that note at the heading, Backspace returns; a click on a heading's `>` folds it; the bytes after `q`, Ctrl+Q, lock and the passphrase prompt hold no `?1003h` after their off sequence; auto-lock still fires with only motion reports sent. Review of the Q1 to Q4 diff (`/security-review`). Done: the binary tests `viewer_clicks_follow_links_and_fold` and `moving_the_mouse_does_not_hold_off_auto_lock`; `/security-review` found nothing at confidence 8 or above, and its one hardening note was taken: input without activity arriving past the limit now locks at once, so a stream of motion cannot keep a wait from ending empty |
| Q5 (done) | Acceptance: setup guide (`view` row, copying with Shift+drag), testing chapter, `scripts/check-mouse.sh` (clicks, hover and folding in Ptyxis by hand, and Ctrl+click or Shift+Ctrl+click on a web link), the status line; fmt, clippy, test, deny, audit, mdbook (long tests and fuzz runs only if the user wants them) | Final review of the whole Q0 to Q5 diff for note text in escape sequences, modes left on, the activity rule, `unsafe`, syscalls or dependencies (expected: none). After the user's check in Ptyxis (Ctrl+click on web links works, clicks on links follow): markers show only under the mouse, bold orange, `v` for an open section and `>` for a folded one, in place of always-shown gray ones |

## Steps J0 to J8 (heading and block suggestions in links; done)

Asked 2026-11-19, after W1 to W6: inside a link, `#` lists the target
note's headings and `#^` its paragraphs, as in Obsidian
([Internal links](https://help.obsidian.md/links)). The user chose A
(headings, and paragraphs that already have a `^id`) and B (any
paragraph: choosing one without an id writes a new `^id` into it, as
Obsidian does), knowing that it adds about 370 to 410 lines of code
plus tests, brings no security benefit, and that B makes Crystal write
a note the user did not open. Rejected (option C): vault-wide `[[##` and
`[[^^`. Scope is fixed. Not included: headings holding `[`, `]`, `|`, `#`
or `^` (left out of the list rather than rewritten), blocks in code,
tables or HTML, and plain-text (`.txt`) notes, which have neither.

Behaviour: `[[name#` lists the headings of the note `name` finds
(`Notes::find`; none when no note or several), `[[#` those of the note
being edited; the text after `#` filters them like note names.
`[[name#^` and `[[#^` list the paragraphs and list items, filtered by
the words of their text, each shown by its first line, its `^id` dimmed
when it has one. Choosing a heading writes `[[name#Heading]]`; a block
with an id, `[[name#^id]]`. A block without one gets a new id, ` ^` and
6 random characters from `a-z0-9`, unique in that note, at the end of
its last line: in the note being edited it goes into the editor's text,
in the same undo step as the link; in another note it is saved at once
through the usual save (`Session::save` with `Change::Body`, the atomic
writer, refused when `vault.age` changed on disk), and only after that
save succeeded is the link written. In the demo notes nothing is saved
and no link is written.

Rules for every step: no new dependency, no new syscall (the ids use
`crypto::fill_random`, already allowed), no change to Landlock, seccomp
or hardening, and no change to `Session::save`, the codec or the atomic
writer: the other note is written only through their existing path. The
list holds note indexes and byte ranges, never copies of note text;
headings and paragraphs are drawn through the grid; none of it reaches an
escape sequence, an error, a log or a panic. Every step is one commit
and leaves Crystal working, ends with fmt, clippy, tests, deny and audit
green, then the security check in its row and a review of that step's
diff for these rules; a check that fails stops the plan until it is
fixed.

| Step | What | Security check before the commit |
| --- | --- | --- |
| J0 (done) | Decision 0075 (security related: Crystal writes into a note the user did not open, and shows another note's text in the editor): when and how the id is written, the save it goes through, demo and conflict refusals, the id's alphabet and source. The architecture chapter's editor part, the leakage chapter (another note's paragraphs on screen in the editor, as `view` already shows them), this plan. Docs only | The decision names every refusal (demo, conflict, a block changed since the list was made) and states that `Session::save` is unchanged; `mdbook build` |
| J1 (done) | `md::blocks(body)`: the paragraphs and list items of a Markdown note, with the same parser and options as the note index: each block's text range, its `^id` if its last line has one (`block_id`, as the index reads it), and where a new id goes (`Block::end`: the end of its last line, before trailing spaces). Code, tables, HTML and headings are not blocks. A list item is its own text, not its nested list. `add_id(body, block, id)` builds the new text in zeroizing memory, parses it again and gives it only when the same blocks come back with the id on the chosen one. Found by the property test and handled: the index's lines end only at `\n`, the parser's also at a lone `\r` (`a\r\r*`: such a block takes no id); and added text can change the parse (`a\n1.` becomes a list once `1.` holds text: `add_id` refuses). Pure, not wired in. About 130 lines | Unit tests (paragraphs, tight and loose lists, nested lists, quotes, `\r\n`, a `^id` already there, text after a code fence). Property tests on random Markdown: every range is a char boundary inside the text; adding ` ^abc123` where a block without id says makes the note index find exactly that id more, and `blocks` then report it on that block alone. A sixth fuzz target `blocks` (its 10-minute run skipped by the user after about 8 minutes clean) |
| J2 (done) | `suggest::target` reads the link being typed with its fragment: `[[name#text`, `[[name#^text`, `[[#text` and `[[#^text`, the name and the text as ranges (`Query`, `Part`); a second `#`, a `^` anywhere but right after `#`, `]`, `[` or `\|` ends it as before. `query` stays the W1 list's (a name with no fragment), so the editor behaves as before until J3. About 50 lines | Unit tests for every form and each character that ends one; the property test (a query is a char-boundary range ending at the cursor) extended to the fragment ranges |
| J3 (done) | Headings: the list's entries index into a `Source` (notes, or one note's headings); `[[name#` lists the found note's headings from the note index (`Notes::headings`), `[[#` and the edited note's own name those of the editor's text (`notes::headings_in`, into zeroizing strings, as the index keeps them), scanned once per link and kept while typing in it: on an 8 MiB note the scan took 185 ms, each key after it about 6 ms. Headings holding `[ ] \| # ^` are left out; a plain-text note has none (`Editor::set_note`, set by `app.rs` on opening and after the first save). Choosing one writes `[[name#Heading]]` and `]]` as for notes; rows are drawn from `Editor::suggestion_label` (`# ` dimmed). About 150 lines | Unit tests (found, ambiguous, missing, folder and `.txt` notes, left-out headings, unsaved own headings, own name, plain text, the scan kept and dropped, a click); a property test that every heading chosen writes a link that resolves, in another note and in the edited one; the drawing property test with heading lists; the editor property test with `#` and headings. Review: the edited note's headings are the only copies, zeroizing, dropped with the list (decision 0075 and the leakage chapter corrected to say so); the canary scan now opens `[[#` on the canary's heading, clean after lock (debug and release) |
| J4 (done) | Blocks: `[[name#^` and `[[#^` list the target's blocks (`Source::Blocks`, `md::blocks` on its saved text or the editor's, kept as ranges in `Editor::blocks`), filtered by the words of their text, each row its first line, the ` ^id` dimmed (labels are now three parts); choosing one with an id writes `[[name#^id]]`, one without shows a notice (J5, J6). Parsed once per link: 137 ms on 8 MiB, each key after it about 4 ms; in the editor's text, typing in the link moves the kept ranges after the link by the change in length, and a block ending on the typed line ends where that line's text does (found by the property test: typing after a trailing space). About 140 lines | Unit tests (listed, filtered, labels, linking by id, the notice, text and missing notes, the edited text's blocks while typing); a property test that every block chosen writes a link that resolves or, without an id, changes nothing; the drawing property test with hostile paragraphs (controls, C1, BiDi, zero-width, tabs): no forbidden character reaches a cell; the editor property test starts in note, heading and block lists and checks the kept blocks equal a fresh parse (300 000 cases once). Review: one unchecked `id.start - 2` made checked; the chosen id is the only copy (zeroizing) |
| J5 (done) | A block without id in the note being edited: a new id (`suggest::new_id`: `fill_random`, 6 characters from `a-z0-9` by rejection sampling of bytes under 252, not one any line of the text already has, at most 8 tries and 64 refills, else a notice) is checked first against the room left and by `add_id` (the note parsed again), then goes in at the block's end and into the link, the later position first, in one undo step; a failure halfway undoes the step. The random source is a field of the editor, `fill_random` outside tests. `syscalls.md` names the new caller of `getrandom`. About 110 lines | Unit tests (ids: alphabet, dropped bytes, taken, failing and empty sources; the block before, after and holding the link; undo and redo; refused by `add_id`, every id taken, no randomness, no room); the block property test with counted ids: 14 bytes more, the link's id is 6 of `a-z0-9` and appears once at a block's end, the link resolves, one Ctrl+Z restores the text; the editor property test reaches new ids with the real source. Review: no note text copied but by `add_id`'s check (zeroizing); the suffix and id on the stack |
| J6 (done) | A block without id in another note: the editor asks the session (`EditorAction::LinkBlock`, a `BlockRequest` of positions and the random id, no note text; mouse and keys both), and `app.rs` finds the block again in the saved text, still without id, has `add_id` check a copy with the id (`NoteText`) and saves it with the unchanged `Session::save(Change::Body)`; only when that save was made does `Editor::link_block` write the link, and only while the cursor is still in that link. Demo notes: nothing saved, nothing written, a notice. A refused or failed save: no link, the notice and the shell message a Ctrl+S failure gives. From the review: `add_id` now refuses a block that is not exactly one of the note's (a kept range ending inside a paragraph would have put ` ^id` where it is not the paragraph's). About 120 lines | Unit tests (the request, taken once, a click too; the link written, one undo step; not written when the cursor left or the link changed; a taken id drawn again; no randomness; that note full; a stale block refused by `add_id`); the block property test saves the id into the other note and checks the link resolves; through the binary: `vault.age` replaced on disk before the choice is refused with the file unchanged, otherwise `links`, unlocked again, finds the new id; demo: `vault.age` byte for byte the same. The canary scan with a block chosen in another note (debug and release). `/security-review` of J1 to J6: no finding at confidence 8 or above; the one low-confidence point fixed by the guard above |
| J7 (done) | Mouse and keys across kinds: clicks, the wheel, Up/Down, Enter/Tab and Esc behave the same for heading and block entries as for notes (one `accept` for all); while a list is open the key line names its kind and keys (`Notes`, `Headings`, `Blocks`, the last saying an ^id is added if missing). About 15 lines | A unit test runs the wheel, arrows, Tab, a click and Esc over heading and block lists; the view test checks the key line for each kind and without a list; the editor property test now sends mouse events too (press, drag, release, wheel, on list rows or text) into note, heading and block lists; through the binary: `a_click_links_a_heading` (the key line names the list, a click on the second row writes `[[plan#Checks]]`, `links` after unlocking again). No security change: drawing goes through the same grid filter, the hint texts are fixed |
| J8 (done) | Acceptance: decision 0045 (the fragment lists, the key line), the setup guide's `edit` row, the testing chapter, `scripts/check-mouse.sh` item 3b (headings and blocks, by hand), the status line; `md::blocks` joins the long property tests (about 6 minutes a million; the run now about 50 minutes). The long tests were stopped at the user's request after md::render, md::safe, md::blocks, ui::wrap, model::name and vault::codec passed; the editor part had found two bugs, both fixed: a link was read across a lone `\r`, where the parser ends a line, so the kept blocks of the edited text went wrong (`suggest::target` now ends lines at `\r` too); a grapheme longer than a grid cell (16 bytes) was laid out 2 columns wide but drawn `?` in 1 (it is now spelled out); both seeds kept, 20 000 cases of each test clean (the `blocks` fuzz hour skipped by the user) | Final review of the whole J0 to J8 diff: no `unsafe`, no dependency (only the fuzz target's entry), no new syscall or file access, every write of another note through `Session::save`; note text reaches no escape sequence, error, log or panic (labels through the grid; non-test slicing bounded by `get`, parser offsets or ASCII finds) |

## Steps W1 to W6 (link suggestions in the editor; done)

Asked 2026-11-19: typing inside `[[` in the editor shows the vault's
notes in a list under the cursor, and typing filters it, as in Obsidian.
The user chose the popup (option A) knowing that it adds code, about 300
to 400 lines with tests, a second input state inside the modeless editor
(decision 0045), and brings no security benefit; the smaller option, Tab
completing inside `[[` like the shell, was declined. Scope is fixed.
Not included: heading (`#`), block (`^`) and alias (`|`) suggestions,
recently opened notes first (Crystal keeps no such history), creating a
note from the list, and suggestions in the shell, viewer or prompts.

Behaviour: the list opens after a key that types or deletes (a
character, Backspace, Delete) leaves the cursor after `[[` (or `![[`) on the
same line, with no `]`, `|`, `#` or `^` between them and the cursor; the
text between is the query. A note matches when every word of the query
is in its name, ignoring case; names whose title starts with the query
come first, then by name (`cmp_ci`); only the best 100 are kept, and typing
more finds the others. An empty query lists every note, up to 100.
At most 8 rows show, scrolled with the highlight. Up and Down move the
highlight (wrapping), Enter or Tab insert, Esc closes the list and typing
opens it again; a typing key edits as usual and filters again, any other
key (arrows, Home, Ctrl keys, the mouse) closes it and acts as usual. No
match closes the list. Inserting replaces the query with the note's
title when no other note has that title, else its full name (`folder/
title`), so the link resolves as `Notes::find` does; the cursor ends after
`]]`, which is added if missing. One Ctrl+Z undoes the insertion.

Rules for every step: no new dependency, no new syscall, no change to
Landlock, seccomp or hardening. The list holds indexes into the names
the session already has (`Notes`), never new copies of note text; names
are drawn through the grid, which drops controls, and the name rules
(0034, 0040) already refuse control, BiDi and invisible characters. No
name reaches an escape sequence, an error, a log or a panic. Every step
is one commit and leaves Crystal working. It ends with fmt, clippy,
tests, deny and audit green, plus the security check in its row. A
check that fails stops the plan until it is fixed. No new decision
entry (not security related): 0045 is brought up to date in place in W6.

| Step | What | Security check before the commit |
| --- | --- | --- |
| W1 (done) | `editor::suggest`, pure functions, not wired in: `query(text, cursor)` finds the open `[[` and the query (or none); `matches(query, names)` filters and orders note indexes; `link_text(notes, index)` gives the title or the full name. About 80 lines | Unit tests (`![[`, a `]`, `|`, `#` or `^` in between, a line break in between, the cursor before `[[`, accents and case); a property test: random text and cursor never panic and the query is always a char-boundary slice inside the text; `link_text` always resolves back to its note through `Notes::find` |
| W2 (done) | `Editor` keeps the list state (`suggest::List`: matching note indexes, highlight, scroll) and `Editor::key_in(key, clipboard, notes)` wraps `key`; while open, Up, Down, Enter, Tab and Esc act on the list, a typing key goes to the text and the list is computed again, any other key closes it. The mouse closes it too (W4 adds clicking rows). Insertion as above, one undo step. The bracket auto-close (B1 to B6) keeps working: `[[` still gives `[[]]`. About 100 lines | Unit tests for each key while open and closed; the editor's property test gains the list keys: cursor and selection stay on grapheme boundaries, the text only changes by typed keys or an insertion of a name; Esc closes, typing reopens; a key the list takes still disarms Ctrl+Q |
| W3 (done) | `View::draw` keeps the cell it drew the cursor in; `View::draw_suggestions(grid, editor, notes)` draws the list over that frame: a box in the code blocks' gray under the cursor's row (above it when there is more room there), at most 8 rows (fewer when fewer fit, the highlight kept shown), from the cursor's column moved left to fit, as wide as the longest shown name, at least 4 columns and at most half the screen, longer names cut with `…`, the folder dimmed, the highlight reversed. Nothing drawn when half the screen is under 4 columns or no row fits. About 90 lines | Tests on the drawn grid: below, above, moved left at the right edge, cutting wide and combining characters, fewer rows than 8, screens too small; a property test over screen sizes, names (wide, combining, emoji) and highlights: the list stays off the title and key lines, never covers the cursor, shows the highlight, and no cell holds a control character |
| W4 (done) | Mouse (decision 0073's reports, no new parser): `View::suggestion_at(column, row)` maps a cell of the last drawn list to its entry; `Editor::mouse_in` wraps `mouse`: a press on an entry links that note, the wheel over the list moves its highlight, a drag or release there does nothing, anything else goes to `mouse`, which closes the list. About 50 lines | Unit tests with presses, the wheel, drags and releases on, beside and past the list, and with it closed; the W3 property test checks that the mouse finds an entry exactly on the gray cells and that the highlighted entry is the reversed row. `position_at` (X3) reads nothing of the list, so its property test is unchanged |
| W5 (done) | Wired in: `app.rs` sends keys through `Editor::key_in` and the mouse through `View::suggestion_at` and `Editor::mouse_in`, and draws the list after the editor (`View::draw_suggestions`), each with the session's `Notes` borrowed, never copied. Ctrl+S is not a typing key, so the list is closed before every save and recomputed from the new names on the next key; lock and Ctrl+Q drop the editor with it. About 15 lines. Measured once: with the list open, a key cost up to 170 ms (release build) in a vault of 100 000 notes, the format's limit, mostly sorting every match; at the user's request ("keep Crystal fast") `matches` keeps only the best 100 (`MAX_MATCHES`, found with `select_nth_unstable_by`, then sorted): up to 45 ms there, the rest being the match itself; a few thousand notes cost a millisecond or two | Through the binary (`link_suggestions_complete_links_by_key_and_click`): `[[ga` and Enter links `garden`, `[[` and a click on the list's second row links `plan`; saved, `links` finds no broken link and the viewer shows both as links. The canary scan's child opens the list on the canary's note, draws it and takes it with Enter: nothing left after lock (debug and release). A unit test that the cut keeps the best 100, not the first, in order. A review of the W1 to W5 diff: no `format!`, `unwrap`, copy of a name or `Debug` outside tests, no `unsafe`, syscall, dependency or change to hardening |
| W6 (done) | Acceptance: decision 0045 states the list in place; the architecture chapter's editor part, the setup guide's editor keys, `scripts/check-mouse.sh` (click a suggestion in Ptyxis), the status line; all checks with `mdbook build`; the long property tests (`scripts/long-tests.sh`) | A final review of the whole diff for names in escape sequences, errors or logs, and for `unsafe`, syscalls or dependencies (expected: none) |

## Steps X0 to X6 (the mouse in the editor)

Approved 2026-11-18 (option C): a click puts the editor's cursor where it
lands, a drag selects, the wheel moves the cursor; double and triple
clicks added 2026-11-19 (X4b) at the user's request. The user chose this
knowing that it adds code, about 250 lines with tests, and brings no
security benefit; earlier refusals: [0041](../decisions.md#0041),
[0052](../decisions.md#0052), [0055](../decisions.md#0055). Scope is
fixed. Not included: scrolling while dragging past the screen's edge
(Shift+arrows extend the selection instead), dragging word by word after
a double click, Shift+click (VTE keeps it for its own selection), and the mouse in the
viewer, the shell or any prompt.

Rules for every step: no new dependency, no new syscall, no change to
Landlock, seccomp or hardening; mouse reports carry only numbers, and no
note text goes into an escape sequence. Every step is one commit and
leaves Crystal working. It ends with fmt, clippy, tests, deny and audit
green, plus the security check in its row. A check that fails stops the
plan until it is fixed.

| Step | What | Security check before the commit |
| --- | --- | --- |
| X0 (done) | Decision 0073: mouse capture in the editor only (mode 1002 for button and drag events, 1006 for SGR reports), never in the viewer, the shell, the lock screen or any prompt; copying out is Shift+drag, the terminal's own selection. 0041's last sentence is cut to point here. The editor chapter, the threat model (A3: a pasted mouse report stays text) and this plan. Docs only | The decision names every place the mode is turned off; `mdbook build` |
| X1 (done) | `shell::keys`: `Key::Mouse { event, column, row }` from `ESC [ < b ; x ; y M/m`. Events: press, drag and release of the left button, wheel up and down; any other button or a modifier held is dropped. Numbers longer than 5 digits, zero or past `u16`, and missing fields drop the report. `MAX_PENDING` grows from 16 to 24, since a full report is 18 bytes. Inside a bracketed paste a report is text. The shell, the pager and questions ignore `Mouse`. About 60 lines. Found by its tests and fixed: a report cut right after its ESC by a full read became Esc plus typed text (a lone ESC is Esc only at the end of a read that did not fill the buffer), and an unknown or too-long CSI sequence left its tail as typed text (CSI now ends at its ECMA-48 final byte, and a too-long one is skipped to it) | Unit tests (reports split across reads, a report inside a paste, every malformed form); a property test that random bytes never panic and the pending buffer stays bounded; a fifth fuzz target `keys` (`src/fuzz.rs`), run for 10 minutes clean |
| X2 (done) | Every place the terminal is handed back or a prompt is shown turns mouse reporting off: `LEAVE_SCREEN` (and so `restore_terminal` after a panic or signal), `CLEAR_SCREEN` (passphrase, lock screen, restore question) and `FULL_REDRAW`. Nothing turns it on yet. Prompts turn reports off, discard waiting input, then show themselves (`discard_input`), so reports sent before go too and nothing typed after the prompt appears is lost (discarding after showing it was tried and lost typed answers). About 20 lines and tests | A test that each of the three byte strings ends mouse reporting; check that prompts still flush pending input (`tcflush` in `term.rs`), so a late report cannot reach a passphrase (where ESC would cancel it) |
| X3 (done) | `editor::View` keeps the area and top row of its last draw. `View::position_at(column, row)` maps a cell back to a byte offset in the text, matching `draw_row`: gutter, tabs, wide characters, hidden characters spelled out as `<U+XXXX>`, past a row's end, past the text's end. `Editor::mouse(event, at)`: a press places the cursor, a drag after a press on the text selects from it (the anchor kept), a release ends that, the wheel moves 3 lines; like a key it ends a typing run and disarms Ctrl+Q. Not wired in yet. About 90 lines | Property tests: every cell maps to a grapheme boundary inside the text; clicking the cell where the cursor is drawn leaves it there; a drag selection is always a valid range. A note with hostile characters maps the same as it draws |
| X4 (done) | Turned on: `Screen` writes `ESC[?1002h ESC[?1006h` while the editor shows and the matching `l` codes when it does not (Ctrl+E preview, Ctrl+Q, lock, suspend). `app.rs` sends the editor's `Mouse` keys to `View` and `Editor`. Clicks on the status line or outside the text are ignored; a drag past the edge stops at the first or last row shown. Auto-lock counts mouse input as activity. About 40 lines. Found in the review: a CSI sequence too long to keep swallowed the ESC of the next one, whose tail became text; skipping now stops at any byte that is not part of a CSI sequence | A test of the bytes `Screen` writes across editor, then preview, then editor, then shell; a test, through the binary where the tests reach the editor, that the bytes after lock contain no turn-on code. Review the X1 to X4 diff (`/security-review`) |
| X4b (done) | Double click selects the word (Unicode word boundaries in its line; a run of spaces or a punctuation mark counts as one), triple click the line with its break, a fourth click starts over: presses at the same place under 400 ms apart, timed in `Editor::mouse` from a time `app.rs` passes in (the monotonic clock, already allowed), so tests set it. A key in between, a later or another place starts over; no drag after a double click. Decision 0073 corrected in place. About 50 lines | Unit test with set times (words, spaces, punctuation, line ends, `\r\n`, accents, a mid-character offset ignored); the property test with random gaps of 0 to 600 ms keeps cursor and selection on grapheme boundaries; the binary test replaces a word and a line |
| X5 (done) | `scripts/check-mouse.sh VAULT`, for what CI cannot click in Ptyxis: click, drag, wheel; Shift+drag copies out what is shown; the wheel still scrolls in the Ctrl+E preview; a click at the lock screen after auto-lock does nothing; Shift+click clears Ptyxis's own selection; after `kill -9`, record what the shell shows and that `reset` clears it. Testing chapter row | The user runs it and reports; any surprise goes back to the step it belongs to |
| X5b (done) | From the user's run of X5: the wheel on the lock screen quit Crystal (VTE sends it as arrow keys there, and any ESC cancelled the prompt): prompts now skip whole escape sequences, only Esc alone cancels (decision 0074, cuts 0016). A selected word seemed to take the space after it: the cursor beside a selection was drawn reversed like the selection; now it is only underlined. About 70 lines | Unit tests and a property test that only typed characters reach a passphrase, split into reads anywhere; Esc, Alt+key and broken sequences cancel; through the binary, arrows at the lock screen and then the passphrase unlock |
| X6 (done) | Acceptance: the setup guide's editor keys, the editor chapter as built, the status line and SUMMARY; all checks with `mdbook build`; the long property tests (`scripts/long-tests.sh`) and the `keys` fuzz target for an hour. In the decoder, an ESC that breaks off a sequence (`ESC [ 1 ESC [ A`) now starts the next one instead of leaving its tail as typed text (reported in the review, fixed at the user's request) | A final review of the whole X0 to X6 diff for note text in escape sequences, for modes left on, and for `unsafe`, syscalls or dependencies (expected: none) |

## Steps K0 to H4 (Phase 8, keys and release)

Approved 2026-11-16 with the user's answers: K4 as one fixed factor, log N 20; K5 dropped; R2 approved; hardware keys wanted (the user owns YubiKeys). Security
first, Crystal minimal. Order: key management (no new dependency, no
sandbox change), then the release work, then hardware keys, whose unlock
design is settled in a decision (H0) before any code. Every step ends
with fmt, clippy, tests, deny and audit green and leaves Crystal working;
tests are automated Rust tests on the real binary where possible, and
anything needing root is a `run0` command the user runs. **Flagged**
marks a step that adds code without a security benefit, or whose cost
is far above its benefit; flagged steps are dropped unless the user
keeps them.

No recovery code (user, 2026-11-16): a forgotten passphrase loses the
notes, on every device, and backups do not help. K0 removes the code and
the commands built on it. Changing the passphrase always makes a new
vault key (user, 2026-11-16: no separate `rotate`): `identity.age`
travels with every sync and backup, so with only a new passphrase every
older copy would still open with the old one and yield the same vault
identity, which opens every later `vault.age`. A user cannot know
whether such a copy is out there, so `passwd` always revokes it.

| Step | What | Why | Size |
| --- | --- | --- | --- |
| K0a (done) | Remove `crystal recover` and `crystal check-code`: `src/recover.rs`, both commands in `cli.rs` and `lib.rs` (the words stay reserved as vault names, so the name rule and the launcher's copy do not change), and their tests (`tests/vault.rs`, `tests/startup.rs`, `tests/sandbox.rs`). `harden::enter_sandbox` and Landlock's `restrict` take the state directory again, not an `Option`: only `check-code` passed none. `VaultIdentity::recipient` is test-only now | The user's choice: one way into the vault and its code less | removes ~600 lines |
| K0b (done) | `init` asks for the passphrase twice and nothing else: no code, no `identity.backup.age`; `src/recovery.rs` and `init::confirm_recovery_code` gone, `init::VAULT_FILES` has three files. `crystal rm` keeps its own list with `identity.backup.age`, so an older vault's backup (and its temp files) is still removed (unit test). An existing `identity.backup.age` is ignored by unlock; the guide will say to delete it by hand, since it still opens the vault with the old code. Tests: `init_asks_only_for_the_passphrase` replaces the code confirmation tests; the pty driver's code-typing markers are gone | Same | removes ~450 lines |
| K0c (done) | The book: decision 0029 deleted and 0064 added (no recovery code; older vaults' `identity.backup.age` deleted by hand), 0004 cut to the vault identity in `identity.age`, 0022, 0027, 0028, 0032, 0036, 0058 and 0059 brought up to date; Storage, Backups, Leakage, threat model (A1, A9 now an old paper), Startup, Sandboxing, Syscalls, Testing, the setup guide (keep the passphrase in a password manager; deleting `identity.backup.age`) and the launcher's comment | The book matches the code | docs |
| K1 | `crystal passwd NAME`: changes the passphrase **and** the vault key (a KEK and DEK change in one; no separate `rotate`). Crash-safe order using age's several recipients, no format change: (1) `vault.age` sealed to the old **and** new recipient (generation +1), (2) `identity.age` (new passphrase, new identity), (3) `vault.age` to the new recipient alone (generation +1). After every step the vault opens with the old or the new passphrase, and a failure says which. Unlock already accepts a two-recipient vault, and the next save writes one recipient | A leaked passphrase or `identity.age` copy opens nothing saved afterwards | ~150 + ~200 tests |
| K1a (done) | `VaultRecipient::encrypt_with` and `format::seal_moving` (two recipients); `src/passwd.rs`: `Rekey` with the three steps, the rollback counter after each `vault.age`, the stamp check (a `vault.age` changed on disk is never replaced), `VERSION` for a format-1 vault, and errors that say which passphrase opens the vault (`Reached`). Seven unit tests: the full change (an old `identity.age` copy opens nothing afterwards), a stop after step 1 and after step 2, failed writes at each step (`FailAt`), a changed `vault.age`, fixed messages. No debug CLI flag is needed: the tests call the steps one at a time. Mutation-checked: step 1 sealed to the new key alone fails three tests | The core, testable without a terminal | ~280 with tests |
| K1b (done) | `crystal passwd NAME` (`cli.rs`, `lib.rs::passwd_vault`): refused before any prompt for a missing vault or a sync-conflict copy, like unlock; the sandbox before the first prompt; the current passphrase, then the new one twice (20 characters, 0028), so no decrypted vault waits on a prompt; then unlock (pin, counter) and, for an older vault, the `restore` question with the auto-lock limit; then `Rekey::run`. `passwd` is not a reserved vault name (the command always comes first, as with `unlock`). Tests: `passwd_changes_the_passphrase`, `passwd_refusals_change_nothing` (wrong current, short new, a cancel at each prompt), `passwd_asks_before_accepting_an_older_vault`, `passwd_waits_inside_the_sandbox` (also in release; mutation-checked), and `passwd` in `commands_check_the_vault_before_prompting` | The command | ~45 + ~150 tests |
| K1c (done) | Decision 0065 (`passwd` changes passphrase and vault key; the three writes; what it cannot take back); 0004, 0028, 0032, 0058 and 0059 name `passwd`; Storage (vault directory, key design), Startup (steps 6 and 10), Sandboxing (section 3), Syscalls (`passwd` reads and checks like unlock), threat model A1 and A5, setup guide ("Changing the passphrase"), roadmap status | The book matches the code | docs |
| K4 (done) | scrypt as **one fixed factor, log N 20** (decision 0066; 1 GiB, about 2 s per unlock), chosen by the user over a per-vault setting. `crypto::ENCRYPT_LOG_N` 20, `harden::MEMLOCK_FLOOR` 2 GiB (half holds scrypt); decryption keeps the absolute cap of log N 22 (`MAX_DECRYPT_LOG_N`), not the factor plus 4, so a planted file cannot ask for 16 GiB. Files at log N 18 still open (unit test); `crystal passwd` rewrites them at 20. Tests: policy table, `passphrase_files_use_log_n_20`, the refusal of log N 21 at the 2 GiB floor; the vault suite runs about twice as long (96 s). 0026 and 0027 cut to their rules; setup guide (2097152 KiB), storage open question closed, threat model A1, startup, sandboxing, syscalls, testing | 4 times the work per passphrase guess | ~15 lines, tests, docs |
| K5 (dropped) | Plaintext export: dropped by the user for now (it would need a change to the rule that only ciphertext is written, a Landlock rule outside the vault, and leaves plaintext on an SSD) | | |
| R1 (done) | Reproducible builds (decision 0067). `packaging/build-release.sh` sets `--remap-path-prefix` for the checkout (`/crystal`), Cargo's registry under both spellings (on secureblue `/home` is a symlink to `/var/home`), and the toolchain's `rust-src` (rustc maps `/rustc/<commit>` back to it when installed) to `/rustc/<commit>`; Cargo's `trim-paths` is unstable on 1.99. `packaging/check-reproducible.sh`: two clean clones of `HEAD`, offline, different directories and Cargo homes; same SHA-256 (`c408d7a0…` for 0ccec15) and no home path left (47 before). Mutation-checked: without the registry remap the hashes differ and 32 paths show. Found: the login name `user` is an ordinary word, so the check looks for paths only | Anyone can check the binary was built from this source; no user name in the binary | ~40 lines script |
| R2 (done, then dropped by decision 0072: Cargo hashes the wrapper's install path, so builds differed between machines) | `cargo auditable`: `cargo-auditable` =0.7.7 (released 2026-10-02, not yanked) installed with `cargo install --locked`; `build-release.sh` runs `cargo auditable build` and refuses to build without the tool. The binary gains a 982-byte `.dep-v0` section; `cargo audit bin` finds 64 dependencies and no advisory; `check-reproducible.sh` still gives identical hashes (`74e2abeb…`) and no home path | A shipped binary can be checked for advisories without its source | ~6 lines script, docs |
| R3 (done) | Signed releases (decision 0069): two `ed25519-sk` keys, one per YubiKey (`crystal-release-1` `SHA256:P39EbN2y…`, `-2` `SHA256:b9cJShmQ…`), made by the user; `packaging/allowed_signers` (principal and namespace `crystal-release`); `packaging/sign-release.sh KEY` refuses uncommitted changes, builds, writes `SHA256SUMS`, signs with a touch and verifies. Acceptance passed with both keys on 7e162e7 (binary `74e2abeb…`, the same as the reproducibility check): genuine files verify; a changed `SHA256SUMS`, a changed binary and the wrong namespace are refused. Open: publishing the keys outside the repository before a public release | A replaced download is detected before install | ~35 lines script, one file |
| R4 (done) | Installation guide: "Getting a binary you can trust" (verify the signature, then the hash; check the keys against a copy published elsewhere, with both fingerprints; or rebuild and compare the hash), then `install` and `restorecon -F`; the setup guide points there. The immutable-flag section stays as it was. The `run0 chattr +i` check is the user's, below | The documented install is the checked one | docs |
| P1 (done) | `.github/workflows/release-build.yml`: CI builds every `v*` tag (and on demand) with `build-release.sh`, read-only token, prints the commit and hash. Its first runs differed from the local build; the host linker was replaced by the toolchain's `rust-lld` (decision 0071) and `cargo-auditable` dropped (decision 0072, its path entered the crate hashes). Since then CI and local both give `833c18b1…` | A second, independent builder before signing | ~40 lines |
| P2 (done) | `packaging/publish-release.sh TAG KEY`: clean tree, `HEAD` is the tag, GitHub's tag is the same commit, CI's run succeeded; builds, compares with CI's hash, signs only if equal, publishes after a `y`. Each refusal tested in a scratch clone | A release cannot be signed from a build no one else reproduced | ~75 lines |
| P3 (done) | Decision 0071 names the script; `development/releasing.md` (the maintainer's procedure); roadmap | The book matches the code | docs |
| H0 | Decision only: how a hardware-key vault unlocks while the sandbox starts before the first prompt (0059) and is never weakened. Proposed: the age plugin (e.g. `age-plugin-yubikey`) is spawned **before** the sandbox, Crystal's only exec (CLAUDE.md allows exactly this), with stdin and stdout pipes, no terminal, stderr discarded and a cleared environment; then Crystal enters the sandbox and does everything else inside it: parsing `identity.age`, the age plugin protocol over the open pipes (a new parser of hostile input: bounded, fuzzed), the PIN and touch prompts on Crystal's own prompt, decryption. The plugin runs in its own SELinux domain (`crystal_plugin_t`, pcscd only), never in `crystal_t`. Lock on such a vault wipes and exits, since no new plugin can be started inside the sandbox. `identity.age` goes to one or more hardware keys (age allows no passphrase beside them); with no recovery code, a lost or broken key loses the notes, so two keys (a second recipient, no new file) are strongly advised. To settle in H0: how Crystal knows the plugin's name before reading `identity.age` (a fixed public file like `VERSION`, versus a header peek outside the sandbox, which would put a parser back outside), and where the plugin binary may come from (not a `PATH` search, A5). Rejected: `execve` in the allowlist, a broker process, a plugin kept alive for the lock screen, and FIDO2, PIV or TPM spoken by Crystal itself (own protocol code, P-256, a construction outside age, 0042) | The design must exist before code, as for 0059 | decision, book |
| H1 to H4 (wanted; YubiKey) | H1 the plugin protocol client (`src/age_v1/plugin.rs`, unwrap side) with a fuzz target and a test-only fake plugin; H2 spawning before the sandbox, the unlock path, lock exits, seccomp (`wait4` to reap the child, if needed) and `syscalls.md`, sandbox tests that `exec` still dies afterwards; H3 `crystal add-key NAME` (the plugin wraps the file key for its recipient); H4 the SELinux plugin domain, built as the user, loaded with `run0 semodule -i`, and a manual check with the real device. Benefit is real but only with a hardware key: a phished passphrase is useless and `identity.age` cannot be guessed offline. Cost: Crystal's first exec, a new hostile-input parser, external software (the plugin), a second SELinux domain, the launcher stays passphrase-only. The user owns YubiKeys and wants it; built after H0 is approved | Asked in the plan | ~850 lines total, ~40 policy |

Order of work: K0a, K0b, K0c, then K1a, K1b, K1c (K2 dropped, K3 folded into K1), K4;
R1 to R4; H0; H1 to H4 after H0 is approved. Phase 8 acceptance: two
clean builds in different directories give identical hashes, and the
signature verifies; also, a `passwd` interrupted after any step still
opens with the old or the new secrets.

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
| S0 (skipped) | Skipped by the user (2026-11-16); the sandbox tests run locally. CI runs only what needs no Landlock (decision 0070, 2026-11-16). Was: CI: a runner or VM with Landlock ABI 9 or newer, never a weaker mode (sandboxing section 9); first a probe printing the runner's kernel and ABI | From S3 on, Crystal refuses to start below ABI 9, so every pty test fails on an older runner. Deferred by the user: the sandbox is tested locally first | ~10 lines YAML, then the fix |
| S1 (done) | Decision 0059 and the book: the sandbox is entered **before the first passphrase prompt**, not after the identity is decrypted; startup steps 8 to 11 and sandboxing sections 1 and 3 reordered. Code comments naming step 8 as unlock follow in S3, with the code | The lock screen already re-unlocks inside the sandbox (0048), so the first unlock needs no syscall the sandbox lacks; moving it in puts the parsers of `identity.age` and `vault.age` (hostile input, A1) and scrypt inside Landlock and seccomp | docs only |
| S2 (done) | Dependencies: `landlock` 0.4.7 (MIT/Apache-2.0; brings `enumflags2`, reuses `libc` and `thiserror`) and `seccompiler` 0.5.0 (Apache-2.0/BSD-3; no default features, so no serde). Exact pins, `deny.toml` checked | Both already chosen in [Dependencies](../design/dependencies.md); Landlock and BPF by hand would need `unsafe` in the crystal crate | Cargo.toml, lock |
| S3 (done) | `src/harden/landlock.rs`: ABI 9 hard requirement, rules from the vault and state fds already open (vault and state: read, write, truncate, make and remove files; no `REFER`, `EXECUTE`, `MAKE_DIR`, `REMOVE_DIR`, `IOCTL_DEV`), TCP bind and connect handled with no rule, signal and abstract-socket scopes. Called from `harden::enter_sandbox` at the point S1 fixes | The file layer: a bug or a bad crate can no longer read `~/.ssh` or other vaults, nor reach D-Bus or Wayland sockets | ~150 lines |
| S4 (done) | Syscall set collected locally: `src/harden/seccomp.rs` (23 syscalls, by number from `libc` =0.2.191, a direct dependency for its constants only, adding no crate) ran with the log action, uncommitted, through the debug and release suites; the audit log (`run0 ausearch`, run by the user) named `fcntl` and `prlimit64`; a positive control (no `fsync`) proved the log works; 7 runtime syscalls were shown unneeded. `fcntl` in debug builds is std's `debug_assert_fd_is_open`; in release only the resume after a suspend. Committed in kill mode; [Syscalls](../reference/syscalls.md) lists the allowlist | The allowlist comes from what Crystal really calls, not from memory | syscalls.md, ~80 lines |
| S5 (done) | `src/harden/seccomp.rs`: kill-process default and argument rules: `ioctl` only the five terminal requests (so `TIOCSTI` and `TIOCLINUX` kill), `kill` only own pid and `SIGSTOP`, `fcntl` only `F_DUPFD_CLOEXEC` and musl's follow-up `F_SETFD FD_CLOEXEC` (plus std's `F_GETFD` in debug builds), `prlimit64` only reading `RLIMIT_MEMLOCK`, `mmap` without `PROT_EXEC`; `mprotect` not allowed at all. No `openat` flag rule: the filter cannot tell callers apart, and Landlock already limits the paths. Unit test: the filter compiles with 23 syscalls. Mutation-checked: no `TCFLSH` kills every prompt, `kill` with another signal kills the suspend | The syscall layer: no `socket`, `execve`, `ptrace`, `io_uring`, `clone` and no `TIOCSTI`, even if Landlock had a hole | ~150 lines |
| S6a (done) | Debug-only `--sandbox-check-for-test CHECK` (the `*-for-test` naming; release refuses it) and `tests/sandbox.rs`: reading `~/.ssh/id_ed25519`, creating a file outside, and reading another vault are denied with `EACCES`; `/proc/PID/status` shows `NoNewPrivs: 1`, `Seccomp: 2`, one filter; `/proc/PID/mem` unreadable. Mutation-checked: without Landlock the three file checks report `ALLOWED`. Found: std's `File::open` makes musl call the old `open` syscall, which seccomp kills, so the checks use rustix `openat` like the rest of Crystal | Proof that the file layer holds, and the status from outside | ~110 + ~120 lines |
| S6b (done) | Seccomp checks, each dying by `SIGSYS` (exit 159): `socket`, `socketpair`, a thread, `memfd_create`, `mkdirat`, `FIONBIO` (an `ioctl` outside the five), `SIGCONT` to the parent, `exec`; unit test that `TIOCSTI` and `TIOCLINUX` are not in the list. Landlock alone (debug-only `harden::enter_landlock_only_for_test`): TCP connect, a live pathname UNIX socket, `mkdirat`, the parent signal and `exec` refused (`EACCES`, `EPERM`). Mutation-checked per check; found that `socketpair`, threads, `memfd_create` and other ioctls are stopped by seccomp only | Each layer must hold even if the other had a hole (sandboxing section 1) | ~200 + ~60 lines |
| S7 (done) | The refusal without Landlock, end to end: debug-only `--hide-landlock-for-test` (a one-rule seccomp filter: `landlock_create_ruleset` gets `ENOSYS`, as from a kernel without Landlock), test `refuses_to_start_without_landlock` (exit 1, the ABI 9 message, no prompt; positive control reaches the prompt). Mutation-checked: best-effort mode is still refused by the status check; without it Crystal reached the prompt and the test failed | Planned as a unit test of the message; the real path is tested instead, without a VM or `unsafe` | ~30 + ~25 lines |
| S8 (done) | `recover` and `check-code` enter the sandbox right before the recovery code prompt, after the data directory is closed; `check-code` (reads, changes nothing) passes no state directory, so Landlock grants the vault alone (`enter_sandbox` takes `state` as an `Option`). The existing pty tests (`recover_*`, `check_code_*`, including the `restore` path) pass inside it; new test `recover_and_check_code_wait_inside_the_sandbox` reads `Seccomp: 2` from outside, also in release; mutation-checked for each command. `init` and `rm` stay outside | Same protection for the other commands that parse `identity.backup.age` | ~25 + ~20 lines |
| S9 (done) | SELinux module in `packaging/selinux/` (decision 0061): `crystal_t`, `crystal_exec_t`, `crystal_vault_t`, `crystal_state_t`; built as the user, installed with `run0`. A full pty session ran in `crystal_t`, permissive first: the one denial (`read` on the home directory) was answered in the code (`O_PATH` for `HOME` and the walk) and the module narrowed (no `filetrans_pattern`, which also granted read and remove on the parents); the same session then logged none, and passed in enforcing mode. Found: `restorecon` needs `-F` (`home_bin_t` is customizable) | A layer Crystal cannot switch off itself | ~110 lines policy, a few lines Rust |
| S10 (done) | `packaging/crystal-sandboxed.sh` (decision 0063): bwrap with all namespaces, only `HOME` in the environment, the binary, this vault and the state directory; Crystal's name rule checked before any path is built; never creates the state directory (wrong label). The module lets `unconfined_bubblewrap_t` enter `crystal_t` (with `nnp_transition`, `nosuid_transition`), found because Crystal would otherwise stay in bwrap's domain. Checked in a scratch HOME: a session works, the other vaults do not exist inside, no network. The `crystal_t` check through bwrap (needs the hardened install) was skipped by the user | An outer layer Crystal does not set up itself | ~60 lines script, ~10 lines policy |
| S11a (done) | Library and program split, no behavior change: `src/main.rs` became `src/lib.rs` (`fn main` is `pub fn run`), a five-line `src/main.rs` calls `crystal::run()`. The modules stay private, so dead code is still reported; nothing else is exported. Found: a library with public modules (tried on a scratch copy first) would have hidden dead code | The fuzz targets need a library to call | ~20 lines |
| S11b (done) | `fuzz/`, its own workspace (decision 0062): `libfuzzer-sys` =0.4.13, `Cargo.lock` started from the root one, `deny.toml` adds NCSA only there; `src/fuzz.rs` under `cfg(fuzzing)` only (render, sanitize, age with decrypt, round-trip and damaged-file modes, vault), plus a fixed X25519 key under the same `cfg`. Stable 1.99 with `--sanitizer none --debug-assertions` works, so no nightly. One hour each, all four in parallel: clean (render 5.7 M runs, sanitize 58 M, age 5.6 M, vault 696 M) | Finds parser bugs property tests miss; the sandbox limits what such a bug can do, fuzzing removes the bug | ~110 + ~80 lines |
| S12 | Book pass done: introduction, roadmap status and plan, `testing.md` (the old sandbox row, no `cargo vet` or `cargo geiger`, the outside review now due), `syscalls.md` (the second table is every use, before and inside the sandbox; the obsolete log-mode paragraph cut), threat model A4 and A6 (no `cargo vet`, which was never set up; the three layers), sandboxing header, leakage. Then `/security-review` of the Phase 6 changes | The book matches the code; the review mirrors Phases 4 and 5 | docs |

Order of work: S1, S2, then S3 to S7 one per turn, then S8, S9, S11,
S12, then S10; S0 skipped. Phase 6 acceptance: every check in
[Sandboxing](../design/sandboxing.md) section 9 passes locally (passed
2026-11-16); CI and the `crystal_t` check through the launcher were
skipped by the user.

## Steps G1 to G5 (code stands out in the viewer; done)

Asked 2026-11-18: code in a gray box with rounded corners. Flagged as no
security benefit (about 80 to 120 lines in `src/md/render.rs` plus
tests); the user chose both parts. It weakens nothing: the block only
arranges already sanitized text and adds fixed padding, every cell goes
through the grid, and the `render` fuzz target covers the new path. After
seeing G3 the user dropped the border: a gray block is enough.
Inline code cannot have rounded corners (a cell is a rectangle, and
Nerd Font half circles are not in standard fonts), so it gets a gray
background. No new decision (not security related).

| Step | What | Why |
| --- | --- | --- |
| G1 (done) | Inline code (`` `x` ``): yellow on a dark gray background (256-color 236), no padding (a padding space could be where a line wraps) | Visible in a line of text |
| G2 (done) | A code block's lines are collected until its end, as a table's cells are, and drawn there; drawn exactly as today, so every existing test passes unchanged | The block needs the widest line first |
| G3 (done) | A gray block: each line after a gray space, padded to the widest line plus one; no border (drawn first, then dropped by the user), so no extra rows and every line keeps its number; fenced and indented blocks alike, an empty block as one gray row | The block itself |
| G4 (done) | A line wider than the screen wraps inside the gray block; with fewer than `MIN_CELL` columns for text it is drawn unpadded (still gray); blocks inside lists and quotes take their prefix | Narrow windows and nesting |
| G5 (done) | Module comment, `dependencies.md` (renderer row), setup guide `view` row, roadmap; all checks, and the `render` fuzz target run (10 minutes) | The book states what is drawn |

## Steps B1 to B6 (editor closes brackets; done)

Asked 2026-11-18: the editor closes brackets as you type, so wikilinks
(`[[` gives `[[]]`) and links (`[text](url)`) close too. Flagged as no
security benefit (about 60 to 100 lines in `src/editor.rs` plus tests);
the user chose the full version. The pairs are `()`, `[]` and `{}`;
quotes and backticks stay unpaired (`don't`). Pastes are never paired.
Code fences get no exception. No new decision (not security related):
0045 is brought up to date in place.

| Step | What | Why |
| --- | --- | --- |
| B1 (done) | Typing `(`, `[` or `{` with no selection inserts the pair, cursor between, but only when the next character is whitespace, a closing bracket or the end of the text (`(word` stays `(word`) | The pairing itself, without getting in the way before a word |
| B2 (done) | Typing `)`, `]` or `}` when the same character follows the cursor steps over it | Otherwise the pair gives `))` |
| B3 (done) | Backspace between an empty pair (`(\|)`) deletes both | Undoes a pair typed by mistake in one key |
| B4 (done) | An opening bracket with a selection wraps it (`[` twice turns `Note` into `[[Note]]`), the wrapped text stays selected | Typing over a selection with `(` would otherwise delete it |
| B5 (done) | Undo: a pair, a step-over, a pair deleted and a wrap are each part of the typing run (one Ctrl+Z); the proptest's keys include the brackets, so cursor and selection stay on grapheme boundaries | The new paths handle every keystroke of note text |
| B6 (done) | Decision 0045 and the setup guide's `edit` row say what the editor closes; roadmap status; all checks | The log states only what is in force |

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
7. **Phase 6: Sandbox** (done; steps S0 to S12 above). Landlock (ABI 9, hard requirement) and a seccomp allowlist with argument rules, entered before the first passphrase prompt (decision 0059); the debug-only `--sandbox-check-for-test CHECK` and `tests/sandbox.rs`; the SELinux module (decision 0061); fuzzing in its own workspace (decision 0062). The optional bubblewrap launcher (S10, decision 0063). CI (S0) skipped.
   - Accept: the checks in [Sandboxing](../design/sandboxing.md) section 9 pass, including that `socket`, `execve` and opening `~/.ssh` are killed or denied (passed locally 2026-11-16; CI skipped).
8. ~~**Phase 7: Backups.**~~ Dropped (decision 0036): backups are left to restic, borg or any file-level tool; Crystal only accepts a restored, older vault after the word `restore` (step L3), and `crystal check-code NAME` checks the recovery code (step L1).
9. **Phase 8: Keys and release.** age plugin unlock (YubiKey or TPM), optionally a second hardware key, passphrase change, a new recovery code, and key rotation (with an optional stronger scrypt setting, see Storage, Open questions), explicit plaintext export with a warning, reproducible builds, signed releases, per-user install to `~/.local/bin` with the optional immutable flag ([Installation](../guide/installation.md)). Steps K0 to H4 above (proposed): no recovery code any more (K0, user 2026-11-16); `passwd` always makes a new vault key (K1); K4 (stronger scrypt) and K5 (plaintext export) are flagged for dropping; hardware keys are settled in a decision (H0) first.
   - Accept: two clean builds produce identical hashes; the signature verifies.

A usable daily driver exists after Phase 4; Phases 5 and 6 are what make it meaningfully more secure than gocryptfs plus an editor.
