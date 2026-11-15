//! The shell's vault commands (decision 0033): `ls`,
//! `tags`, `find`, `search`, `links`, and the ones that leave the shell:
//! `view` (the pager) and `edit` (the editor, decision 0045, which also
//! creates a missing note, decision 0038). Each writes its answer into the
//! shell's scrollback, which sanitizes it; every line holding note text is
//! built in a zeroizing buffer of its final size.

use zeroize::Zeroizing;

use crate::md::render::{Kind, shown};
use crate::model::name::{check_folder, check_segment, unique_key};
use crate::notes::{Found, Notes, cmp_ci};
use crate::shell::{ERROR_STYLE, Shell};
use crate::ui::grid::{Color, Mods, Style};

const FOLDER_STYLE: Style = Style::PLAIN.fg(Color::Blue).mods(Mods::BOLD);
const NAME_STYLE: Style = Style::PLAIN.fg(Color::Cyan);
const QUIET: Style = Style::PLAIN.mods(Mods::DIM);

/// Most results `find` shows.
pub const MAX_FOUND: usize = 20;
/// Most lines `search` shows, and per note.
pub const MAX_HITS: usize = 100;
pub const MAX_HITS_PER_NOTE: usize = 3;
/// Characters of context `search` shows before a match.
const CONTEXT: usize = 30;
/// Characters of a matching line `search` shows.
const SNIPPET: usize = 100;

/// What a command hands over to, leaving the shell.
pub enum Open {
    View(usize),
    Edit(usize),
    /// `edit` of a note that does not exist yet: its name passed the
    /// vault's rules. The caller asks "Create it?" and the type.
    Create {
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
    },
    /// `mv`: the note and its new name, which passed the vault's rules.
    Move {
        note: usize,
        folder: Zeroizing<String>,
        title: Zeroizing<String>,
    },
    /// `rm`: the note to delete, once the user confirms.
    Remove(usize),
}

/// Runs `words` (the first is the command); unknown commands are reported.
/// Returns where to go when the command was `view`, `edit`, `mv` or `rm`.
pub fn run(notes: &Notes, words: &[Zeroizing<String>], shell: &mut Shell) -> Option<Open> {
    let args: Vec<&str> = words.iter().skip(1).map(|w| w.as_str()).collect();
    let rest = joined(&args);
    match words.first().map(|w| w.as_str()) {
        Some("ls") => ls(notes, &args, shell),
        Some("tags") => tags(notes, shell),
        Some("find") => find(notes, &rest, shell),
        Some("search") => search(notes, &rest, shell),
        Some("links") => links(notes, shell),
        Some("view") => return view(notes, &rest, shell).map(Open::View),
        Some("edit") => return edit(notes, &rest, shell),
        Some("mv") => return mv(notes, &args, shell),
        Some("rm") => return view(notes, &rest, shell).map(Open::Remove),
        Some(other) => say(
            shell,
            &["unknown command: ", other, " (type help)"],
            ERROR_STYLE,
        ),
        None => {}
    }
    None
}

/// `edit NAME`: the note NAME names; when there is none, a name that
/// follows the vault's rules (`FOLDER/TITLE` or `TITLE`) for a note to
/// create, once the user answers (decision 0038). It is typed without
/// `.txt`: the type is asked, not typed. A bare title that one note has
/// opens that note, so a new top-level note cannot take a title that a
/// note in a folder has alone (create it under another name, then `mv`).
fn edit(notes: &Notes, name: &str, shell: &mut Shell) -> Option<Open> {
    if name.is_empty() {
        shell.print(
            "Which note? (edit NAME; a missing one is created)",
            ERROR_STYLE,
        );
        return None;
    }
    match find_shown(notes, name) {
        Found::One(index) => Some(Open::Edit(index)),
        Found::Ambiguous(list) => {
            ambiguous(notes, list, shell);
            None
        }
        Found::None if shown(name).1 == Kind::Plain => {
            shell.print(
                "No note with that name. Leave out .txt: Crystal asks for the type.",
                ERROR_STYLE,
            );
            None
        }
        Found::None => {
            let (folder, title) = free_name(notes, name, None, shell)?;
            Some(Open::Create { folder, title })
        }
    }
}

/// The title a new note is stored under: `title` itself for Markdown, with
/// `.txt` for text (decision 0038). `None`, said
/// in the shell, when the `.txt` makes it too long.
pub fn stored_title(title: &str, kind: Kind, shell: &mut Shell) -> Option<Zeroizing<String>> {
    let mut stored = Zeroizing::new(String::with_capacity(title.len() + 4));
    stored.push_str(title);
    if kind == Kind::Plain {
        stored.push_str(".txt");
    }
    if let Err(e) = check_segment(&stored) {
        say(shell, &["Not a valid name: ", &e.to_string()], ERROR_STYLE);
        return None;
    }
    Some(stored)
}

/// `mv OLD NEW`: the note OLD names, and a free name NEW (the same note in
/// another case is fine).
fn mv(notes: &Notes, args: &[&str], shell: &mut Shell) -> Option<Open> {
    let [old, new] = args else {
        shell.print(
            "mv OLD NEW (quote a name with spaces: mv \"old name\" \"new name\")",
            ERROR_STYLE,
        );
        return None;
    };
    let note = view(notes, old, shell)?;
    // The type stays (decision 0038): NEW is a shown name, and a text
    // note keeps its hidden `.txt`.
    if shown(new).1 == Kind::Plain {
        shell.print(
            "Leave out .txt: mv cannot change a note's type.",
            ERROR_STYLE,
        );
        return None;
    }
    let kind = shown(notes.name(note)).1;
    let mut stored = Zeroizing::new(String::with_capacity(new.len() + 4));
    stored.push_str(new);
    if kind == Kind::Plain {
        stored.push_str(".txt");
    }
    if notes.name(note) == stored.as_str() {
        shell.print("That is its name already.", ERROR_STYLE);
        return None;
    }
    let (folder, title) = free_name(notes, &stored, Some(note), shell)?;
    // No two notes may show the same name: `todo` beside `todo.txt`.
    let mut twin = Zeroizing::new(String::with_capacity(title.len() + 4));
    match kind {
        Kind::Plain => twin.push_str(shown(&title).0),
        Kind::Markdown => {
            twin.push_str(&title);
            twin.push_str(".txt");
        }
    }
    if notes.exists(&folder, &twin) {
        shell.print("A note with that name exists already.", ERROR_STYLE);
        return None;
    }
    Some(Open::Move {
        note,
        folder,
        title,
    })
}

/// `name` split into folder and title, if it follows the vault's rules and
/// no note other than `except` has it, ignoring case.
fn free_name(
    notes: &Notes,
    name: &str,
    except: Option<usize>,
    shell: &mut Shell,
) -> Option<(Zeroizing<String>, Zeroizing<String>)> {
    let (folder, title) = name.rsplit_once('/').unwrap_or(("", name));
    // check_folder takes "" for the top level, but in `/x` the `/` promises
    // a folder: refuse it rather than make the top-level `x`.
    let checked = if name.starts_with('/') {
        check_segment(folder)
    } else {
        check_folder(folder)
    };
    if let Err(e) = checked.and_then(|()| check_segment(title)) {
        say(shell, &["Not a valid name: ", &e.to_string()], ERROR_STYLE);
        return None;
    }
    let is_except = except.is_some_and(|note| {
        let n = notes.note(note);
        unique_key(folder, title) == unique_key(n.folder.as_str(), n.title.as_str())
    });
    if notes.exists(folder, title) && !is_except {
        shell.print("A note with that name exists already.", ERROR_STYLE);
        return None;
    }
    Some((
        Zeroizing::new(folder.to_owned()),
        Zeroizing::new(title.to_owned()),
    ))
}

/// The note `view NAME` or `edit NAME` names, or why there is none.
fn view(notes: &Notes, name: &str, shell: &mut Shell) -> Option<usize> {
    if name.is_empty() {
        shell.print("Which note? (view NAME, edit NAME)", ERROR_STYLE);
        return None;
    }
    match find_shown(notes, name) {
        Found::One(index) => Some(index),
        Found::None => {
            shell.print("No note with that name (try find).", ERROR_STYLE);
            None
        }
        Found::Ambiguous(list) => {
            ambiguous(notes, list, shell);
            None
        }
    }
}

/// The notes a bare title could mean.
fn ambiguous(notes: &Notes, list: Vec<usize>, shell: &mut Shell) {
    shell.print("More than one note has that title; which one?", ERROR_STYLE);
    for index in list {
        named(shell, "  ", notes.name(index), Style::PLAIN);
    }
}

/// A note by the name the shell shows (decision 0038): `todo` also finds
/// the text note stored as `todo.txt`, when no note is named `todo`.
/// Typing `.txt` still works. Links keep Obsidian's rule (`[[todo.txt]]`),
/// so `Notes::find` is not changed.
fn find_shown(notes: &Notes, name: &str) -> Found {
    match notes.find(name) {
        Found::None if shown(name).1 == Kind::Markdown => {
            let mut stored = Zeroizing::new(String::with_capacity(name.len() + 4));
            stored.push_str(name);
            stored.push_str(".txt");
            notes.find(&stored)
        }
        found => found,
    }
}

/// `links`: every `[[link]]` that leads nowhere, and why.
fn links(notes: &Notes, shell: &mut Shell) {
    let broken = notes.broken_links();
    if broken.is_empty() {
        shell.print("No broken links.", QUIET);
    }
    for (from, target, why) in broken {
        say(
            shell,
            &[
                shown(notes.name(from)).0,
                ": [[",
                target,
                "]] ",
                why.describe(),
            ],
            Style::PLAIN,
        );
    }
}

/// Prints `parts` as one line, built at its final size.
fn say(shell: &mut Shell, parts: &[&str], style: Style) {
    let mut line = Zeroizing::new(String::with_capacity(parts.iter().map(|p| p.len()).sum()));
    for part in parts {
        line.push_str(part);
    }
    shell.print(&line, style);
}

/// `prefix` and a note's shown name (without the `.txt` of a text note),
/// its type dimmed after it: `Linux Kernel · markdown` (decision 0038).
fn named(shell: &mut Shell, prefix: &str, name: &str, style: Style) {
    let (name, kind) = shown(name);
    let mut line = Zeroizing::new(String::with_capacity(prefix.len() + name.len()));
    line.push_str(prefix);
    line.push_str(name);
    shell.print_labeled(&line, style, kind.label());
}

/// Orders notes by shown name, ignoring case.
fn cmp_shown(a: &str, b: &str) -> std::cmp::Ordering {
    cmp_ci(shown(a).0, shown(b).0)
}

/// A folder in the tree `ls` shows.
struct Dir<'a> {
    name: &'a str,
    dirs: Vec<Dir<'a>>,
    notes: Vec<&'a str>,
}

impl<'a> Dir<'a> {
    fn new(name: &'a str) -> Self {
        Self {
            name,
            dirs: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn insert(&mut self, mut segments: std::str::Split<'a, char>, title: &'a str) {
        match segments.next() {
            None => self.notes.push(title),
            Some(segment) => {
                let at = match self
                    .dirs
                    .iter()
                    .position(|d| cmp_ci(d.name, segment).is_eq())
                {
                    Some(at) => at,
                    None => {
                        self.dirs.push(Dir::new(segment));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[at].insert(segments, title);
            }
        }
    }

    fn sort(&mut self) {
        self.dirs.sort_by(|a, b| cmp_ci(a.name, b.name));
        self.notes.sort_by(|a, b| cmp_shown(a, b));
        for dir in &mut self.dirs {
            dir.sort();
        }
    }

    /// The folder at `path` (case-insensitive), if any.
    fn find(&self, path: &str) -> Option<&Dir<'a>> {
        let mut dir = self;
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            dir = dir.dirs.iter().find(|d| {
                d.name
                    .chars()
                    .flat_map(char::to_lowercase)
                    .eq(segment.chars().flat_map(char::to_lowercase))
            })?;
        }
        Some(dir)
    }

    /// Folders first, then notes, two spaces of indent per level. Depth is
    /// bounded by the name rules (32 levels).
    fn print(&self, depth: usize, shell: &mut Shell) {
        let indent = &"                                                                "
            [..(2 * depth).min(64)];
        for dir in &self.dirs {
            say(shell, &[indent, dir.name, "/"], FOLDER_STYLE);
            dir.print(depth + 1, shell);
        }
        for note in &self.notes {
            named(shell, indent, note, Style::PLAIN);
        }
    }
}

/// `ls`, `ls FOLDER`, `ls #TAG`.
fn ls(notes: &Notes, args: &[&str], shell: &mut Shell) {
    if notes.len() == 0 {
        shell.print("No notes yet.", QUIET);
        return;
    }
    if let Some(tag) = args.first().filter(|a| a.starts_with('#')) {
        let mut names: Vec<&str> = notes.tagged(tag).iter().map(|&i| notes.name(i)).collect();
        if names.is_empty() {
            shell.print("No note has that tag.", QUIET);
        }
        names.sort_by(|a, b| cmp_shown(a, b));
        for name in names {
            named(shell, "", name, Style::PLAIN);
        }
        return;
    }
    let mut root = Dir::new("");
    for index in 0..notes.len() {
        let note = notes.note(index);
        let folder = note.folder.as_str();
        let segments = if folder.is_empty() {
            "".split('/')
        } else {
            folder.split('/')
        };
        let mut segments = segments;
        if folder.is_empty() {
            // `"".split('/')` yields one empty segment: skip it.
            segments.next();
        }
        root.insert(segments, note.title.as_str());
    }
    root.sort();
    match args.first() {
        None => root.print(0, shell),
        Some(path) => match root.find(path) {
            Some(dir) => dir.print(0, shell),
            None => shell.print("No folder with that name.", ERROR_STYLE),
        },
    }
}

/// `tags`: every tag and how many notes have it.
fn tags(notes: &Notes, shell: &mut Shell) {
    let tags = notes.tags();
    if tags.is_empty() {
        shell.print("No tags yet.", QUIET);
    }
    for tag in tags {
        let count = tag.notes.len().to_string();
        say(shell, &["#", &tag.shown, "  ", &count], NAME_STYLE);
    }
}

/// How well `query` (lowercase chars) matches `name` as a subsequence:
/// `None` if it does not; more for letters in a row and at word starts.
fn fuzzy(name: &str, query: &[char]) -> Option<i64> {
    let mut score = 0;
    let mut wanted = query.iter().peekable();
    let mut previous_matched = false;
    let mut previous = '/';
    for c in name.chars().flat_map(char::to_lowercase) {
        match wanted.peek() {
            Some(&&q) if q == c => {
                wanted.next();
                score += 1;
                if previous_matched {
                    score += 3;
                }
                if matches!(previous, '/' | ' ' | '-' | '_') {
                    score += 2;
                }
                previous_matched = true;
            }
            Some(_) => {
                previous_matched = false;
                score -= 1;
            }
            None => break,
        }
        previous = c;
    }
    wanted.peek().is_none().then_some(score)
}

/// The arguments joined by spaces, in a wiped buffer sized once (they
/// can be note text: a search for a line of a note).
fn joined(args: &[&str]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(
        args.iter().map(|a| a.len() + 1).sum(),
    ));
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(arg);
    }
    out
}

fn lowercase_chars(text: &str) -> Zeroizing<Vec<char>> {
    Zeroizing::new(text.chars().flat_map(char::to_lowercase).collect())
}

/// `find TEXT`: names that hold TEXT's letters in order, best first.
fn find(notes: &Notes, query: &str, shell: &mut Shell) {
    let query = lowercase_chars(query.trim());
    if query.is_empty() {
        shell.print("find what? (find TEXT)", ERROR_STYLE);
        return;
    }
    let mut found: Vec<(i64, usize)> = (0..notes.len())
        .filter_map(|i| fuzzy(shown(notes.name(i)).0, &query).map(|score| (score, i)))
        .collect();
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| cmp_shown(notes.name(a.1), notes.name(b.1)))
    });
    if found.is_empty() {
        shell.print("No matching names.", QUIET);
    }
    for &(_, index) in found.iter().take(MAX_FOUND) {
        named(shell, "", notes.name(index), Style::PLAIN);
    }
    if found.len() > MAX_FOUND {
        let more = (found.len() - MAX_FOUND).to_string();
        say(shell, &["and ", &more, " more"], QUIET);
    }
}

/// Where `needle` (lowercase chars) starts in `hay`, ignoring case,
/// without making a lowercase copy of `hay`. Worst case is
/// `hay.len() * needle.len()` (a long query almost matching everywhere);
/// accepted, since only the user's own query against their own notes can
/// cause it, and `search` stops at `MAX_HITS`.
pub fn find_ci(hay: &str, needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.char_indices().map(|(at, _)| at).find(|&at| {
        let mut chars = hay[at..].chars().flat_map(char::to_lowercase);
        needle.iter().all(|n| chars.next() == Some(*n))
    })
}

/// The part of `line` around byte `at`, at most `SNIPPET` characters.
fn snippet(line: &str, at: usize) -> &str {
    let start = line[..at]
        .char_indices()
        .rev()
        .nth(CONTEXT)
        .map_or(0, |(i, _)| i);
    let end = line[start..]
        .char_indices()
        .nth(SNIPPET)
        .map_or(line.len(), |(i, _)| start + i);
    &line[start..end]
}

/// `search TEXT`: every note whose name or text holds TEXT, ignoring case,
/// with the matching lines.
fn search(notes: &Notes, query: &str, shell: &mut Shell) {
    let needle = lowercase_chars(query.trim());
    if needle.is_empty() {
        shell.print("search for what? (search TEXT)", ERROR_STYLE);
        return;
    }
    let mut hits = 0;
    let mut notes_hit = 0;
    for index in 0..notes.len() {
        if hits >= MAX_HITS {
            break;
        }
        let name = notes.name(index);
        let name_hit = find_ci(shown(name).0, &needle).is_some();
        let mut lines = notes
            .note(index)
            .body
            .as_str()
            .lines()
            .filter_map(|line| find_ci(line, &needle).map(|at| snippet(line, at)))
            .take(MAX_HITS_PER_NOTE.min(MAX_HITS - hits))
            .peekable();
        if !name_hit && lines.peek().is_none() {
            continue;
        }
        notes_hit += 1;
        named(shell, "", name, NAME_STYLE);
        for line in lines {
            say(shell, &["  ", line.trim()], Style::PLAIN);
            hits += 1;
        }
    }
    if notes_hit == 0 {
        shell.print("Nothing found.", QUIET);
    } else if hits >= MAX_HITS {
        shell.print("(more not shown; narrow the search)", QUIET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::tests::{note, notes, sample};
    use crate::shell::Shell;
    use crate::ui::grid::Grid;
    use crate::vault::VaultName;

    /// The output of `line` as text rows (the screen is tall enough).
    fn output(notes: &Notes, line: &str) -> Vec<String> {
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let words = crate::shell::parse::words(line).unwrap();
        let viewed = run(notes, &words, &mut shell);
        assert!(viewed.is_none() || line.starts_with("view"));
        let mut grid = Grid::new(80, 40);
        shell.draw(&mut grid);
        // The output rows: the prompt follows them (decision 0049), so it
        // is left out by its text, not by its row.
        (0..40)
            .map(|y| {
                (0..80)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .filter(|row| !row.is_empty() && row != "crystal:diary>")
            .collect()
    }

    #[test]
    fn ls_shows_a_tree() {
        assert_eq!(
            output(&sample(), "ls"),
            [
                "home/",
                "  ideas · markdown",
                "work/",
                "  projects/",
                "    Roadmap · markdown",
                "  meeting 2026-11-07 · markdown",
                "ideas · markdown",
                "todo · text"
            ]
        );
        assert_eq!(
            output(&sample(), "ls WORK"),
            [
                "projects/",
                "  Roadmap · markdown",
                "meeting 2026-11-07 · markdown"
            ]
        );
        assert_eq!(
            output(&sample(), "ls work/projects/"),
            ["Roadmap · markdown"]
        );
        assert_eq!(
            output(&sample(), "ls nowhere"),
            ["No folder with that name."]
        );
        assert_eq!(output(&notes(vec![]), "ls"), ["No notes yet."]);
    }

    #[test]
    fn ls_by_tag_and_tags() {
        assert_eq!(
            output(&sample(), "ls #work"),
            [
                "work/meeting 2026-11-07 · markdown",
                "work/projects/Roadmap · markdown"
            ]
        );
        assert_eq!(output(&sample(), "ls #nope"), ["No note has that tag."]);
        assert_eq!(
            output(&sample(), "tags"),
            ["#crystal  1", "#Home  1", "#idea/sub  1", "#Work  2"]
        );
    }

    #[test]
    fn find_is_fuzzy_and_ranked() {
        let found = output(&sample(), "find rdmp");
        assert_eq!(found, ["work/projects/Roadmap · markdown"]);
        let found = output(&sample(), "find ideas");
        assert_eq!(
            found,
            ["ideas · markdown", "home/ideas · markdown"],
            "the closer match first"
        );
        // The hidden `.txt` is not matched (decision 0038).
        assert_eq!(output(&sample(), "find todo"), ["todo · text"]);
        assert_eq!(output(&sample(), "find txt"), ["No matching names."]);
        assert_eq!(output(&sample(), "find zzz"), ["No matching names."]);
        assert_eq!(output(&sample(), "find"), ["find what? (find TEXT)"]);
    }

    #[test]
    fn search_shows_matching_lines() {
        let found = output(&sample(), "search \"plan FOR\"");
        assert_eq!(
            found,
            [
                "work/projects/Roadmap · markdown",
                "  Plan for #Work and #crystal."
            ]
        );
        let found = output(&sample(), "search IDEAS");
        assert_eq!(
            found,
            [
                "work/projects/Roadmap · markdown",
                "  See [[ideas]].",
                "ideas · markdown",
                "home/ideas · markdown",
                "  Other ideas, #Home."
            ]
        );
        assert_eq!(output(&sample(), "search qqq"), ["Nothing found."]);
    }

    #[test]
    fn search_is_bounded_and_snips_long_lines() {
        let long = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
        let many: Vec<_> = (0..200u8)
            .map(|i| {
                note(
                    i,
                    "",
                    &format!("n{i}"),
                    &format!("needle\nneedle\nneedle\nneedle\n{long}"),
                )
            })
            .collect();
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let words = crate::shell::parse::words("search needle").unwrap();
        run(&notes(many), &words, &mut shell);
        // Each note: its name and at most 3 lines; exactly 100 lines in
        // all, then one line saying more were not shown.
        let shown = shell.output_len();
        let names = MAX_HITS.div_ceil(MAX_HITS_PER_NOTE);
        assert_eq!(shown, MAX_HITS + names + 1);
        assert!(shown > MAX_HITS, "{shown}");
        let line = snippet(&long, 500);
        assert!(
            line.starts_with('a') && line.contains("needle") && line.chars().count() <= SNIPPET
        );
    }

    #[test]
    fn case_insensitive_matching() {
        let needle = lowercase_chars("ÜBER");
        assert_eq!(find_ci("Das ist über alles", &needle), Some(8));
        assert_eq!(find_ci("nothing", &needle), None);
        assert!(fuzzy("work/projects/Roadmap", &lowercase_chars("wpr")).is_some());
        assert_eq!(fuzzy("abc", &lowercase_chars("cba")), None);
    }

    /// The shown name finds a text note without its `.txt`; a Markdown
    /// note of that name comes first, and `.txt` typed still works
    /// (decision 0038).
    #[test]
    fn shown_names_find_text_notes() {
        let n = notes(vec![
            note(1, "work", "todo.txt", ""),
            note(2, "", "plan", ""),
            note(3, "", "plan.txt", ""),
        ]);
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let mut open = |line: &str| run(&n, &crate::shell::parse::words(line).unwrap(), &mut shell);
        assert!(matches!(open("view work/todo"), Some(Open::View(0))));
        assert!(matches!(open("view TODO"), Some(Open::View(0))));
        assert!(matches!(open("view work/todo.txt"), Some(Open::View(0))));
        assert!(matches!(open("view plan"), Some(Open::View(1))));
        assert!(matches!(open("view plan.txt"), Some(Open::View(2))));
        assert!(open("view work/todo.txt.txt").is_none());
    }

    /// `edit` finds a note as `view` does; a missing one is offered for
    /// creation when its name follows the rules and no note has it, typed
    /// without `.txt` (decision 0038).
    #[test]
    fn edit_opens_or_offers_to_create() {
        let n = sample();
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let mut open = |line: &str| run(&n, &crate::shell::parse::words(line).unwrap(), &mut shell);
        assert!(matches!(open("edit roadmap"), Some(Open::Edit(0))));
        match open("edit work/fresh idea") {
            Some(Open::Create { folder, title }) => {
                assert_eq!((folder.as_str(), title.as_str()), ("work", "fresh idea"));
            }
            _ => panic!("edit refused a good name"),
        }
        assert!(matches!(
            open("edit Linux Kernel"),
            Some(Open::Create { .. })
        ));
        assert_eq!(
            output(&sample(), "edit plain.txt"),
            ["No note with that name. Leave out .txt: Crystal asks for the type."]
        );
        assert!(output(&sample(), "edit a:b")[0].starts_with("Not a valid name: "));
        assert!(output(&sample(), "edit bad/")[0].starts_with("Not a valid name: "));
        // A leading `/` is an empty folder, not the top level.
        for line in ["edit /x", "edit //x", "mv roadmap /x"] {
            assert!(
                output(&sample(), line)[0].starts_with("Not a valid name: "),
                "{line}"
            );
        }
        assert_eq!(
            output(&sample(), "edit"),
            ["Which note? (edit NAME; a missing one is created)"]
        );
        assert_eq!(
            output(&sample(), "new x"),
            ["unknown command: new (type help)"]
        );
    }

    /// The stored title: `.txt` only for text, and refused when the
    /// `.txt` makes it longer than 255 bytes.
    #[test]
    fn stored_titles() {
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let plain = stored_title("todo", Kind::Plain, &mut shell).unwrap();
        assert_eq!(plain.as_str(), "todo.txt");
        let md = stored_title("todo", Kind::Markdown, &mut shell).unwrap();
        assert_eq!(md.as_str(), "todo");
        let long = "x".repeat(252);
        assert!(stored_title(&long, Kind::Markdown, &mut shell).is_some());
        assert!(stored_title(&long, Kind::Plain, &mut shell).is_none());
    }

    proptest::proptest! {
        /// A name `edit` offers to create is the name typed: folder and
        /// title join back to it exactly (found by review: `/x` became `x`).
        #[test]
        fn edit_keeps_the_name_as_typed(name in "[a/ .]{1,12}") {
            let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
            let words = [Zeroizing::new("edit".to_owned()), Zeroizing::new(name.clone())];
            if let Some(Open::Create { folder, title }) = run(&sample(), &words, &mut shell) {
                let joined = if folder.is_empty() {
                    title.to_string()
                } else {
                    format!("{}/{}", folder.as_str(), title.as_str())
                };
                proptest::prop_assert_eq!(joined, name);
            }
        }
    }

    /// `mv` needs two names, a note for the first and a free, valid name
    /// for the second (the same note in another case is fine); `rm` finds
    /// its note as `view` does.
    #[test]
    fn mv_and_rm_check_their_names() {
        let n = sample();
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let mut open = |line: &str| run(&n, &crate::shell::parse::words(line).unwrap(), &mut shell);
        match open("mv roadmap \"work/plans/Road map\"") {
            Some(Open::Move {
                note: 0,
                folder,
                title,
            }) => assert_eq!(
                (folder.as_str(), title.as_str()),
                ("work/plans", "Road map")
            ),
            _ => panic!("mv refused a good name"),
        }
        assert!(matches!(
            open("mv work/projects/Roadmap work/projects/ROADMAP"),
            Some(Open::Move { note: 0, .. })
        ));
        assert!(matches!(open("rm roadmap"), Some(Open::Remove(0))));
        assert!(open("rm nope").is_none());
        assert_eq!(
            output(&sample(), "mv roadmap ideas"),
            ["A note with that name exists already."]
        );
        assert_eq!(
            output(&sample(), "mv roadmap work/projects/Roadmap"),
            ["That is its name already."]
        );
        assert!(output(&sample(), "mv roadmap a:b")[0].starts_with("Not a valid name: "));
        assert!(output(&sample(), "mv roadmap")[0].starts_with("mv OLD NEW"));
    }

    /// `mv` keeps the type (decision 0038): a text note keeps its hidden
    /// `.txt`, a typed `.txt` is refused, and no rename makes two notes
    /// show the same name.
    #[test]
    fn mv_keeps_the_type() {
        let n = || {
            notes(vec![
                note(1, "", "todo.txt", ""),
                note(2, "", "plan", ""),
                note(3, "work", "ideas", ""),
            ])
        };
        let moved = |line: &str| {
            let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
            match run(&n(), &crate::shell::parse::words(line).unwrap(), &mut shell) {
                Some(Open::Move {
                    note,
                    folder,
                    title,
                }) => Some((note, format!("{}|{}", folder.as_str(), title.as_str()))),
                _ => None,
            }
        };
        assert_eq!(moved("mv todo done"), Some((0, "|done.txt".to_owned())));
        assert_eq!(
            moved("mv todo work/Todo"),
            Some((0, "work|Todo.txt".to_owned()))
        );
        assert_eq!(moved("mv todo TODO"), Some((0, "|TODO.txt".to_owned())));
        assert_eq!(
            moved("mv plan \"my plan\""),
            Some((1, "|my plan".to_owned()))
        );
        let said = |line: &str| output(&n(), line);
        assert_eq!(
            said("mv todo done.txt"),
            ["Leave out .txt: mv cannot change a note's type."]
        );
        assert_eq!(
            said("mv plan plan.txt"),
            ["Leave out .txt: mv cannot change a note's type."]
        );
        assert_eq!(said("mv todo todo"), ["That is its name already."]);
        // A Markdown `todo` beside the text `todo`, and the reverse.
        assert_eq!(
            said("mv plan todo"),
            ["A note with that name exists already."]
        );
        assert_eq!(
            said("mv todo plan"),
            ["A note with that name exists already."]
        );
        assert!(said(&format!("mv todo {}", "x".repeat(252)))[0].starts_with("Not a valid name: "));
    }

    #[test]
    fn view_resolves_names_and_links_lists_broken_ones() {
        let n = sample();
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let mut view =
            |line: &str| match run(&n, &crate::shell::parse::words(line).unwrap(), &mut shell) {
                Some(Open::View(note)) => Some(note),
                _ => None,
            };
        assert_eq!(view("view roadmap"), Some(0));
        assert_eq!(view("view \"work/meeting 2026-11-07\""), Some(1));
        assert_eq!(view("view ideas"), Some(2), "a full name wins");
        assert_eq!(view("view nope"), None);
        assert_eq!(view("copy roadmap"), None, "no copy command");
        assert_eq!(
            output(&sample(), "view nope"),
            ["No note with that name (try find)."]
        );
        let twice = notes(vec![note(1, "a", "x", ""), note(2, "b", "x", "")]);
        assert_eq!(
            output(&twice, "view x"),
            [
                "More than one note has that title; which one?",
                "  a/x · markdown",
                "  b/x · markdown"
            ]
        );
        assert_eq!(output(&sample(), "links"), ["No broken links."]);
        let broken = notes(vec![note(1, "", "a", "[[missing]] [[a#Nowhere]]")]);
        assert_eq!(
            output(&broken, "links"),
            [
                "a: [[missing]] no note with that name",
                "a: [[a#Nowhere]] that note has no such heading"
            ]
        );
    }

    #[test]
    fn unknown_commands() {
        assert_eq!(
            output(&sample(), "frob x"),
            ["unknown command: frob (type help)"]
        );
    }
}
