//! Line numbers as Crystal shows them, in the viewer and the editor alike
//! (decision 0055): only lines with text are numbered, counted from 1; a
//! blank line (only spaces and tabs before its line break) has no number
//! and is not counted, and neither has a code fence (```` ``` ````,
//! ```` ```rust ````, `~~~`) or a table's `|---|` line, which the viewer
//! does not show. `\n` ends a line, and so does `\r\n`.
//!
//! The same rule works on the raw note (the editor) and the sanitized one
//! (the viewer): the sanitizer turns a tab into spaces and `\r\n` into
//! `\n`, and spells out a lone `\r`, which is text either way.

/// Whether `line`, with its line break if it has one, is blank: only
/// spaces and tabs, a code fence or a table's `|---|` line.
pub fn is_blank(line: &str) -> bool {
    let body = match line.strip_suffix('\n') {
        Some(body) => body.strip_suffix('\r').unwrap_or(body),
        None => line,
    };
    body.bytes().all(|b| b == b' ' || b == b'\t') || is_fence(body) || is_table_rule(body)
}

/// `line` after the list marker it starts with (`-`, `*` or `+`, or up
/// to 9 digits then `.` or `)`), which a space or tab must follow.
fn strip_list_marker(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let marker = match line.as_bytes().get(digits) {
        Some(b'-' | b'*' | b'+') if digits == 0 => 1,
        Some(b'.' | b')') if (1..=9).contains(&digits) => digits + 1,
        _ => return None,
    };
    let rest = &line[marker..];
    rest.starts_with([' ', '\t']).then_some(rest)
}

/// Whether `line` is the line under a table's header: after `>` quote
/// markers, only `|`, `-`, `:`, spaces and tabs, with a `|` and a `-`. A
/// `---` rule has no `|`, so it stays text.
fn is_table_rule(line: &str) -> bool {
    let body = line.trim_start_matches([' ', '\t', '>']);
    body.contains('|')
        && body.contains('-')
        && body
            .bytes()
            .all(|b| matches!(b, b'|' | b'-' | b':' | b' ' | b'\t'))
}

/// Whether `line` opens or closes a fenced code block: after indentation,
/// `>` quote markers and list markers (`-`, `*`, `+`, `1.`, `1)`), three
/// or more `` ` `` (the rest holding none) or three or more `~`.
fn is_fence(line: &str) -> bool {
    let mut body = line.trim_start_matches([' ', '\t', '>']);
    while let Some(rest) = strip_list_marker(body) {
        body = rest.trim_start_matches([' ', '\t', '>']);
    }
    let Some(mark) = body.chars().next().filter(|c| matches!(c, '`' | '~')) else {
        return false;
    };
    let rest = body.trim_start_matches(mark);
    body.len() - rest.len() >= 3 && !(mark == '`' && rest.contains('`'))
}

/// How many lines of `text` are numbered: the last number.
pub fn count(text: &str) -> usize {
    text.split_inclusive('\n')
        .filter(|line| !is_blank(line))
        .count()
}

/// The line numbered `number` of `text`, without its line break; empty if
/// there is none.
pub fn numbered(text: &str, number: u32) -> &str {
    let index = usize::try_from(number).unwrap_or(usize::MAX);
    let Some(line) = text
        .split_inclusive('\n')
        .filter(|line| !is_blank(line))
        .nth(index.wrapping_sub(1))
    else {
        return "";
    };
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Numbers the lines of one text, asked for in mostly increasing order:
/// counts on from its last answer, from the start when asked for an
/// earlier byte.
#[derive(Default)]
pub struct Counter {
    /// The start of the line of the last answer.
    start: usize,
    /// The non-blank lines before `start`.
    before: u32,
}

impl Counter {
    /// The number of the line holding byte `at` of `text` (a byte past the
    /// end is on the last line, or on an empty one after a final line
    /// break); `None` if that line is blank.
    pub fn number(&mut self, text: &str, at: usize) -> Option<u32> {
        let at = at.min(text.len());
        if at < self.start || self.start > text.len() {
            *self = Self::default();
        }
        let mut start = self.start;
        let mut before = self.before;
        for line in text[start..].split_inclusive('\n') {
            let end = start + line.len();
            if at < end || !line.ends_with('\n') {
                self.start = start;
                self.before = before;
                return (!is_blank(line)).then(|| before.saturating_add(1));
            }
            if !is_blank(line) {
                before = before.saturating_add(1);
            }
            start = end;
        }
        // Past a final line break: an empty last line.
        self.start = start;
        self.before = before;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_are_spaces_and_tabs_only() {
        for blank in ["", "\n", "  \t\n", " \r\n", "\t"] {
            assert!(is_blank(blank), "{blank:?}");
        }
        // A lone `\r` is text, as the sanitizer spells it out.
        for text in ["a", " a\n", "\r", " \r", "\u{a0}\n", "\r\r\n"] {
            assert!(!is_blank(text), "{text:?}");
        }
        // Code fences count as blank; inline code and short runs do not.
        for fence in ["```", "```rust\n", "  ~~~~ py\r\n", "> ```", "````"] {
            assert!(is_blank(fence), "{fence:?}");
        }
        // Also on a list item's line, however nested.
        for fence in [
            "- ```rust",
            "* ~~~",
            "+\t```",
            "12. ```",
            "3) ```",
            "> - 1. ```",
        ] {
            assert!(is_blank(fence), "{fence:?}");
        }
        for text in [
            "``", "```x```", "a ```", "~~", "-```", "1```", "a. ```", "- x ```",
        ] {
            assert!(!is_blank(text), "{text:?}");
        }
        // So do tables' `|---|` lines; a `---` rule or a row does not.
        for rule in ["|---|", "| :-- | --: |\n", "> |-|-|", "--- | ---"] {
            assert!(is_blank(rule), "{rule:?}");
        }
        for text in ["---", "| a |", "|", "| - x |"] {
            assert!(!is_blank(text), "{text:?}");
        }
    }

    #[test]
    fn numbers_skip_blank_lines() {
        let text = "# T\r\n\n  \none\n\ttwo\n\n";
        assert_eq!(numbered(text, 1), "# T");
        assert_eq!(numbered(text, 2), "one");
        assert_eq!(numbered(text, 3), "\ttwo");
        assert_eq!(numbered(text, 4), "");
        assert_eq!(numbered(text, 0), "");
        assert_eq!(count(text), 3);
        assert_eq!(count(""), 0);
        let mut lines = Counter::default();
        let at = |s: &str| text.find(s).unwrap();
        assert_eq!(lines.number(text, 0), Some(1));
        assert_eq!(lines.number(text, 3), Some(1), "the \\r of \\r\\n");
        assert_eq!(lines.number(text, 5), None, "a blank line");
        assert_eq!(lines.number(text, at("one") + 2), Some(2));
        assert_eq!(lines.number(text, at("two")), Some(3));
        assert_eq!(lines.number(text, text.len()), None);
        // Going back counts again from the start.
        assert_eq!(lines.number(text, at("one")), Some(2));
        assert_eq!(lines.number(text, 1), Some(1));
        assert_eq!(Counter::default().number("x", 9), Some(1));
        assert_eq!(Counter::default().number("", 0), None);
    }
}
