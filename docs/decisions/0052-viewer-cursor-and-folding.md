# 0052 The viewer has a keyboard cursor and folds sections

- **Date:** 2026-11-14 (after Phase 5)
- **Status:** Accepted. The cursor's dark-gray line highlight is
  replaced by a `>` in a gutter with line numbers
  ([0053](0053-viewer-gutter-and-line-copy.md)).

## Context

Long notes are hard to get around in the viewer (`view NAME`): it
scrolled, and Tab selected `[[links]]`, but it showed no structure.
The user asked for Org-mode-style headings (an icon and a color per
level, sections indented under their heading) and for folding a heading
with Tab. The viewer had no cursor, so "the heading I am on" meant
nothing; the user first asked for a cursor moved by mouse clicks.

## Decision

- **Headings:** `◉ ○ ● ✿ ◆ ◇` in yellow, green, red, blue, magenta and
  cyan for levels 1 to 6, in place of the dimmed `#` marks. A heading
  outside quotes and lists opens a section: the heading is indented two
  columns per level below the first, and what follows it two more, with
  quote bars, list markers and tables inside that indent. A wrapped
  heading lines up under its text. A heading inside a quote or list gets
  its icon but opens no section. Icons and indents are fixed text; the
  indent is dropped where it would leave no room, as for lists.
- **A keyboard cursor, read-only:** a highlighted line (dark-gray
  background; each cell keeps its colors, and dark-gray quote bars and
  table borders turn gray). j/k and the arrows move it by a line, Space
  and PgDn/PgUp by a screen (page and cursor together), g/G and
  Home/End to the ends; the screen scrolls only as far as needed to keep
  it in view. It changes nothing in the note.
- **Tab folds:** on a heading line, Tab hides its section, up to the next
  heading of the same or a higher level, and marks the heading ` …`; Tab
  again shows it. Elsewhere Tab only says "Move the cursor to a heading to
  fold it." Folds nest: an inner fold stays folded when the outer one
  opens. Moving, paging and the ends count shown lines only. A link
  selected inside a fold, or a `#Heading` or `^block` jump landing in
  one, opens the folds around it. Folds belong to the note on screen:
  never saved, cleared when another note opens or Backspace goes back,
  kept across a resize.
- **n and N select links** (next and previous), starting from the cursor
  when none is selected, and move the cursor to the link; Enter follows
  it as before. Shift+Tab does nothing in the viewer.
- **No mouse.** Crystal still does not capture the mouse
  ([0041](0041-internal-clipboard-only.md),
  [0045](0045-simple-editor.md)).
- The key line reads "q shell · j/k move · Tab fold · n/N links · Enter
  follow · Backspace previous · …", so the keys up to Backspace fit 80
  columns.

## Alternatives

- **A cursor moved by mouse clicks** (the user's first request):
  capturing the mouse takes plain drag-selection away from the terminal,
  the only way note text reaches another program
  ([0041](0041-internal-clipboard-only.md)); it adds a parser for mouse
  reports next to the bracketed-paste parser
  ([0047](0047-paste-report.md)), and mouse modes to undo on every exit
  (quit, lock, auto-lock, SIGTSTP, panic). Declined.
- **Tab folds the section at the top of the screen, no cursor:** less
  state, but the target is implicit. The user chose a cursor.
- **An outline view (one key shows only headings):** one flag instead of
  per-heading state, about a third of the code; the user chose full
  per-heading folding.
- **Saving folds with the note:** would change the vault format for view
  state. Folds are not saved.

## Consequences

- About 250 lines in the renderer and the viewer, plus tests: the
  renderer records sections; the viewer's movement goes through a few
  functions that skip folded lines (`next_shown`, `prev_shown`, `step`),
  each one pass over the folds.
- No new state outside the viewer, no new syscall, no new input path;
  the escape corpus still renders inert (its golden files show the new
  icons and indents).
- Tab no longer selects links in the viewer; muscle memory from before
  needs n.
- Sections indent the text, so deep levels take width from it on a
  narrow screen; Obsidian does not indent sections.
