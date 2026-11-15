//! Line numbers as Crystal shows them, in the viewer and the editor alike
//! (decision 0055): only lines with text are numbered, counted from 1; a
//! blank line (only spaces and tabs before its line break) has no number
//! and is not counted. `\n` ends a line, and so does `\r\n`.
//!
//! The same rule works on the raw note (the editor) and the sanitized one
//! (the viewer): the sanitizer turns a tab into spaces and `\r\n` into
//! `\n`, and spells out a lone `\r`, which is text either way.

/// Whether `line`, with its line break if it has one, is blank.
pub fn is_blank(line: &str) -> bool {
    let body = match line.strip_suffix('\n') {
        Some(body) => body.strip_suffix('\r').unwrap_or(body),
        None => line,
    };
    body.bytes().all(|b| b == b' ' || b == b'\t')
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
