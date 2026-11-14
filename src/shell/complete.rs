//! Tab completion for the shell (docs/decisions/0033-shell.md): command
//! words in the first word, note names and folders after it, ignoring
//! case. Names complete one folder level at a time (`wo` → `work/`), and a
//! name with spaces is quoted, the way `parse` reads it back.

use zeroize::Zeroizing;

/// Every command word, for completing the first word.
pub const COMMANDS: [&str; 14] = [
    "clear", "edit", "exit", "find", "help", "links", "ls", "mv", "new", "quit", "rm", "search",
    "tags", "view",
];

/// The word the cursor is at the end of.
#[derive(Debug, PartialEq, Eq)]
pub struct Word {
    /// Where the word starts in the line, its opening quote included.
    pub start: usize,
    /// The word as typed, without the quote and with escapes undone.
    pub text: Zeroizing<String>,
    /// Whether it is the line's first word (a command).
    pub first: bool,
}

/// The word before `cursor`, read as `parse::words` reads it: spaces end a
/// word unless inside quotes, a backslash takes the next character.
pub fn word_at(line: &str, cursor: usize) -> Word {
    let before = &line[..cursor];
    let mut start = 0;
    let mut text = Zeroizing::new(String::with_capacity(before.len()));
    let mut words_before = 0;
    let mut quoted = false;
    let mut in_word = false;
    let mut chars = before.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            ' ' if !quoted => {
                if in_word {
                    words_before += 1;
                    in_word = false;
                }
                text.clear();
            }
            '"' => {
                if !in_word {
                    start = at;
                    in_word = true;
                }
                quoted = !quoted;
            }
            '\\' => {
                if !in_word {
                    start = at;
                    in_word = true;
                }
                if let Some((_, next)) = chars.next() {
                    text.push(next);
                }
            }
            c => {
                if !in_word {
                    start = at;
                    in_word = true;
                }
                text.push(c);
            }
        }
    }
    if !in_word {
        start = cursor;
    }
    Word {
        start,
        text,
        first: words_before == 0,
    }
}

/// How many bytes of `candidate` match `prefix` ignoring case, or `None`.
fn prefix_len_ci(candidate: &str, prefix: &str) -> Option<usize> {
    let mut wanted = prefix.chars().flat_map(char::to_lowercase).peekable();
    for (at, c) in candidate.char_indices() {
        if wanted.peek().is_none() {
            return Some(at);
        }
        for lower in c.to_lowercase() {
            if wanted.next() != Some(lower) {
                return None;
            }
        }
    }
    wanted.peek().is_none().then_some(candidate.len())
}

/// The candidates for `prefix`, one folder level deep: a name below a
/// further `/` shows as its folder (`work/`). Sorted, without repeats
/// (ignoring case).
pub fn options<'a>(prefix: &str, candidates: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut found: Vec<&str> = candidates
        .filter_map(|c| {
            let matched = prefix_len_ci(c, prefix)?;
            Some(match c[matched..].find('/') {
                Some(slash) => &c[..matched + slash + 1],
                None => c,
            })
        })
        .collect();
    found.sort_by(|a, b| crate::notes::cmp_ci(a, b));
    found.dedup_by(|a, b| {
        a.chars()
            .flat_map(char::to_lowercase)
            .eq(b.chars().flat_map(char::to_lowercase))
    });
    found
}

/// What all `options` share at the start, ignoring case, as the first one
/// spells it.
pub fn shared_prefix<'a>(options: &[&'a str]) -> &'a str {
    let Some(first) = options.first() else {
        return "";
    };
    let mut end = first.len();
    for other in &options[1..] {
        end = end.min(
            first
                .char_indices()
                .zip(other.chars())
                .find(|((_, a), b)| !a.to_lowercase().eq(b.to_lowercase()))
                .map_or(first.len().min(other.len()), |((at, _), _)| at),
        );
    }
    let mut end = end.min(first.len());
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    &first[..end]
}

/// `text` as the shell reads it back: quoted if it holds a space, a quote
/// or a backslash (those two escaped); the closing quote only if `whole`.
pub fn quoted(text: &str, whole: bool) -> Zeroizing<String> {
    let needs = text.contains([' ', '"', '\\']);
    let mut out = Zeroizing::new(String::with_capacity(text.len() * 2 + 2));
    if needs {
        out.push('"');
    }
    for c in text.chars() {
        if needs && matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    if needs && whole {
        out.push('"');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_under_the_cursor() {
        let w = word_at("vi", 2);
        assert_eq!((w.start, w.text.as_str(), w.first), (0, "vi", true));
        let w = word_at("view wo", 7);
        assert_eq!((w.start, w.text.as_str(), w.first), (5, "wo", false));
        let w = word_at("view \"meeting 2", 15);
        assert_eq!((w.start, w.text.as_str(), w.first), (5, "meeting 2", false));
        let w = word_at("view ", 5);
        assert_eq!((w.start, w.text.as_str(), w.first), (5, "", false));
        let w = word_at("view a\\ b", 9);
        assert_eq!((w.start, w.text.as_str()), (5, "a b"));
    }

    #[test]
    fn options_go_one_level_at_a_time() {
        let names = [
            "work/projects/Roadmap",
            "work/meeting 2026-11-07",
            "Work/other",
            "welcome",
            "ideas",
        ];
        // `Work/` and `work/` are one folder, ignoring case; the first
        // spelling in sort order is shown.
        assert_eq!(options("w", names.iter().copied()), ["welcome", "Work/"]);
        assert_eq!(
            options("WORK/", names.iter().copied()),
            ["work/meeting 2026-11-07", "Work/other", "work/projects/"]
        );
        assert_eq!(options("x", names.iter().copied()), Vec::<&str>::new());
        assert_eq!(options("vi", COMMANDS.iter().copied()), ["view"]);
    }

    #[test]
    fn shared_prefixes() {
        assert_eq!(shared_prefix(&["work/", "welcome"]), "w");
        assert_eq!(shared_prefix(&["meeting 1", "Meeting 2"]), "meeting ");
        assert_eq!(shared_prefix(&["日本a", "日本b"]), "日本");
        assert_eq!(shared_prefix(&["only"]), "only");
        assert_eq!(shared_prefix(&[]), "");
    }

    #[test]
    fn quoting_round_trips_through_parse() {
        for name in ["plain", "two words", "a\"b", "c\\d", "日本 語"] {
            let typed = quoted(name, true);
            let back = crate::shell::parse::words(&typed).unwrap();
            assert_eq!(back[0].as_str(), name, "{typed:?}");
        }
        assert_eq!(quoted("two words", false).as_str(), "\"two words");
        assert_eq!(quoted("plain", false).as_str(), "plain");
    }
}
