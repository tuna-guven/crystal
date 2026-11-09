//! The shell's vault commands (docs/decisions/0033-shell.md), read-only:
//! `ls`, `tags`, `find`, `search`, `links`, and `view` (which hands the
//! note to the pager). Each writes its answer into the
//! shell's scrollback, which sanitizes it; every line holding note text is
//! built in a zeroizing buffer of its final size.

use zeroize::Zeroizing;

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

/// Runs `words` (the first is the command); unknown commands are reported.
/// Returns the note to show when the command was `view`.
pub fn run(notes: &Notes, words: &[Zeroizing<String>], shell: &mut Shell) -> Option<usize> {
    let args: Vec<&str> = words.iter().skip(1).map(|w| w.as_str()).collect();
    let rest = joined(&args);
    match words.first().map(|w| w.as_str()) {
        Some("ls") => ls(notes, &args, shell),
        Some("tags") => tags(notes, shell),
        Some("find") => find(notes, &rest, shell),
        Some("search") => search(notes, &rest, shell),
        Some("links") => links(notes, shell),
        Some("view") => return view(notes, &rest, shell),
        Some(other) => say(
            shell,
            &["unknown command: ", other, " (type help)"],
            ERROR_STYLE,
        ),
        None => {}
    }
    None
}

/// The note `view NAME` names, or why there is none.
fn view(notes: &Notes, name: &str, shell: &mut Shell) -> Option<usize> {
    if name.is_empty() {
        shell.print("view which note? (view NAME)", ERROR_STYLE);
        return None;
    }
    match notes.find(name) {
        Found::One(index) => Some(index),
        Found::None => {
            shell.print("No note with that name (try find).", ERROR_STYLE);
            None
        }
        Found::Ambiguous(list) => {
            shell.print("More than one note has that title; which one?", ERROR_STYLE);
            for index in list {
                say(shell, &["  ", notes.name(index)], Style::PLAIN);
            }
            None
        }
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
            &[notes.name(from), ": [[", target, "]] ", why.describe()],
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
        self.notes.sort_by(|a, b| cmp_ci(a, b));
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
            say(shell, &[indent, note], Style::PLAIN);
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
        names.sort_by(|a, b| cmp_ci(a, b));
        for name in names {
            say(shell, &[name], Style::PLAIN);
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
        .filter_map(|i| fuzzy(notes.name(i), &query).map(|score| (score, i)))
        .collect();
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| cmp_ci(notes.name(a.1), notes.name(b.1)))
    });
    if found.is_empty() {
        shell.print("No matching names.", QUIET);
    }
    for &(_, index) in found.iter().take(MAX_FOUND) {
        say(shell, &[notes.name(index)], Style::PLAIN);
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
        let named = find_ci(name, &needle).is_some();
        let mut lines = notes
            .note(index)
            .body
            .as_str()
            .lines()
            .filter_map(|line| find_ci(line, &needle).map(|at| snippet(line, at)))
            .take(MAX_HITS_PER_NOTE.min(MAX_HITS - hits))
            .peekable();
        if !named && lines.peek().is_none() {
            continue;
        }
        notes_hit += 1;
        say(shell, &[name], NAME_STYLE);
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
        (0..39)
            .map(|y| {
                (0..80)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .filter(|row| !row.is_empty())
            .collect()
    }

    #[test]
    fn ls_shows_a_tree() {
        assert_eq!(
            output(&sample(), "ls"),
            [
                "home/",
                "  ideas",
                "work/",
                "  projects/",
                "    Roadmap",
                "  meeting 2026-11-07",
                "ideas",
                "todo.txt"
            ]
        );
        assert_eq!(
            output(&sample(), "ls WORK"),
            ["projects/", "  Roadmap", "meeting 2026-11-07"]
        );
        assert_eq!(output(&sample(), "ls work/projects/"), ["Roadmap"]);
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
            ["work/meeting 2026-11-07", "work/projects/Roadmap"]
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
        assert_eq!(found, ["work/projects/Roadmap"]);
        let found = output(&sample(), "find ideas");
        assert_eq!(found, ["ideas", "home/ideas"], "the closer match first");
        assert_eq!(output(&sample(), "find zzz"), ["No matching names."]);
        assert_eq!(output(&sample(), "find"), ["find what? (find TEXT)"]);
    }

    #[test]
    fn search_shows_matching_lines() {
        let found = output(&sample(), "search \"plan FOR\"");
        assert_eq!(
            found,
            ["work/projects/Roadmap", "  Plan for #Work and #crystal."]
        );
        let found = output(&sample(), "search IDEAS");
        assert_eq!(
            found,
            [
                "work/projects/Roadmap",
                "  See [[ideas]].",
                "ideas",
                "home/ideas",
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

    #[test]
    fn view_resolves_names_and_links_lists_broken_ones() {
        let n = sample();
        let mut shell = Shell::new(&VaultName::new("diary".into()).unwrap());
        let mut view = |line: &str| run(&n, &crate::shell::parse::words(line).unwrap(), &mut shell);
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
                "  a/x",
                "  b/x"
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
