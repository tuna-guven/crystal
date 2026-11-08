//! Splitting a shell line into words (docs/decisions/0033-shell.md): spaces
//! separate words, double quotes group a name with spaces, and a backslash
//! takes the next character literally (`\"`, `\\`). Nothing else is
//! special: no pipes, redirection, globbing, variables or substitution,
//! and nothing is ever run outside Crystal.

use std::fmt;

use zeroize::Zeroizing;

/// Fixed text; never the line.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    UnclosedQuote,
    TrailingBackslash,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnclosedQuote => "a quote is not closed",
            Self::TrailingBackslash => "the line ends with a backslash",
        })
    }
}

impl std::error::Error for ParseError {}

/// The words of `line`, each in its own zeroizing buffer (they can be note
/// names or search text).
pub fn words(line: &str) -> Result<Vec<Zeroizing<String>>, ParseError> {
    let mut words = Vec::new();
    let mut word: Option<Zeroizing<String>> = None;
    let mut quoted = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next = chars.next().ok_or(ParseError::TrailingBackslash)?;
                word.get_or_insert_with(|| new_word(line)).push(next);
            }
            '"' => {
                quoted = !quoted;
                // `""` is an empty word.
                word.get_or_insert_with(|| new_word(line));
            }
            ' ' if !quoted => {
                if let Some(done) = word.take() {
                    words.push(done);
                }
            }
            c => word.get_or_insert_with(|| new_word(line)).push(c),
        }
    }
    if quoted {
        return Err(ParseError::UnclosedQuote);
    }
    words.extend(word);
    Ok(words)
}

/// A word buffer that never grows: no word is longer than the line.
fn new_word(line: &str) -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(line.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(line: &str) -> Result<Vec<String>, ParseError> {
        words(line).map(|w| w.iter().map(|s| s.as_str().to_owned()).collect())
    }

    #[test]
    fn spaces_split_words() {
        assert_eq!(split("ls").unwrap(), ["ls"]);
        assert_eq!(
            split("  view   work/roadmap  ").unwrap(),
            ["view", "work/roadmap"]
        );
        assert!(split("").unwrap().is_empty());
        assert!(split("   ").unwrap().is_empty());
    }

    #[test]
    fn quotes_and_backslashes() {
        assert_eq!(
            split(r#"view "meeting 2026-11-07""#).unwrap(),
            ["view", "meeting 2026-11-07"]
        );
        assert_eq!(split(r#"a"b c"d"#).unwrap(), ["ab cd"]);
        assert_eq!(
            split(r#"say \"hi\" a\\b"#).unwrap(),
            ["say", "\"hi\"", "a\\b"]
        );
        assert_eq!(split(r#"x "" y"#).unwrap(), ["x", "", "y"]);
        assert_eq!(split(r"one\ word").unwrap(), ["one word"]);
    }

    #[test]
    fn nothing_else_is_special() {
        assert_eq!(
            split("ls | rm * > x $HOME `id` ;").unwrap(),
            ["ls", "|", "rm", "*", ">", "x", "$HOME", "`id`", ";"]
        );
    }

    #[test]
    fn errors() {
        assert_eq!(split(r#"view "open"#), Err(ParseError::UnclosedQuote));
        assert_eq!(split(r"view x\"), Err(ParseError::TrailingBackslash));
        let shown = ParseError::UnclosedQuote.to_string();
        assert!(!shown.contains("open"));
    }
}
