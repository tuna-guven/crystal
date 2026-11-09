//! Note names (docs/decisions/0034-vault-format-2.md): a note's name is
//! `folder/title`, or `title` at the top level. Folders exist only as this
//! metadata. The rules are Obsidian's, so a link `[[name]]` can always
//! name the note, and every name is a valid file name for an export:
//!
//! - a title or folder segment is 1 to 255 bytes, has no leading or
//!   trailing whitespace, is not `.` or `..`, and holds none of
//!   `[ ] # ^ | \ / : * ? " < >`, no control or BiDi character, and no
//!   invisible character that could make two names look the same
//!   (docs/decisions/0040-invisible-name-characters.md);
//! - a folder is `""` or segments joined by `/`, at most 32 levels and
//!   1 KiB;
//! - names are unique in a vault, compared case-insensitively (as Obsidian
//!   resolves links).
//!
//! Errors are fixed text: never the name itself, which is note content.

use std::fmt;

use zeroize::Zeroizing;

use crate::ui::grid::is_forbidden;

pub const MAX_SEGMENT_BYTES: usize = 255;
pub const MAX_FOLDER_BYTES: usize = 1024;
pub const MAX_FOLDER_LEVELS: usize = 32;

/// Characters Obsidian does not allow in a note name (link syntax and
/// file-system reserved characters).
const RESERVED: [char; 13] = [
    '[', ']', '#', '^', '|', '\\', '/', ':', '*', '?', '"', '<', '>',
];

/// Invisible characters with no use in a name: zero-width space, word
/// joiner, byte-order mark, soft hyphen, Mongolian vowel separator. Zero-
/// width joiner and non-joiner stay allowed (emoji sequences, Persian).
const INVISIBLE: [char; 5] = ['\u{200B}', '\u{2060}', '\u{FEFF}', '\u{AD}', '\u{180E}'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    /// Leading or trailing whitespace, `.` or `..`.
    BadForm,
    /// A reserved, control or BiDi character.
    BadCharacter,
    /// The folder has too many levels or bytes.
    FolderTooDeep,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "a note title or folder name is empty",
            Self::TooLong => "a note title or folder name is longer than 255 bytes",
            Self::BadForm => {
                "a note title or folder name starts or ends with a space, or is . or .."
            }
            Self::BadCharacter => {
                "a note title or folder name holds one of [ ] # ^ | \\ / : * ? \" < >, a \
                 control character or an invisible one"
            }
            Self::FolderTooDeep => "a folder is deeper than 32 levels or longer than 1 KiB",
        })
    }
}

impl std::error::Error for NameError {}

/// A title, or one segment of a folder.
pub fn check_segment(segment: &str) -> Result<(), NameError> {
    if segment.is_empty() {
        return Err(NameError::Empty);
    }
    if segment.len() > MAX_SEGMENT_BYTES {
        return Err(NameError::TooLong);
    }
    if segment.starts_with(char::is_whitespace)
        || segment.ends_with(char::is_whitespace)
        || segment == "."
        || segment == ".."
    {
        return Err(NameError::BadForm);
    }
    if segment.contains(|c| RESERVED.contains(&c) || INVISIBLE.contains(&c) || is_forbidden(c)) {
        return Err(NameError::BadCharacter);
    }
    Ok(())
}

/// `""` (top level) or segments joined by `/`.
pub fn check_folder(folder: &str) -> Result<(), NameError> {
    if folder.is_empty() {
        return Ok(());
    }
    if folder.len() > MAX_FOLDER_BYTES || folder.split('/').count() > MAX_FOLDER_LEVELS {
        return Err(NameError::FolderTooDeep);
    }
    folder.split('/').try_for_each(check_segment)
}

/// The key that makes names unique: the full name, lowercased. It is note
/// content, so it lives in zeroizing memory, and has no Debug.
#[derive(PartialEq, Eq, Hash)]
pub struct NameKey(KeyText);

/// `Zeroizing<String>` with `Hash`, which zeroize does not provide.
#[derive(PartialEq, Eq)]
struct KeyText(Zeroizing<String>);

impl std::hash::Hash for KeyText {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.as_str().hash(state);
    }
}

impl NameKey {
    pub fn as_str(&self) -> &str {
        &self.0.0
    }
}

pub fn unique_key(folder: &str, title: &str) -> NameKey {
    let mut key = Zeroizing::new(String::with_capacity(folder.len() + 1 + title.len()));
    for part in [folder, "/", title] {
        for c in part.chars().flat_map(char::to_lowercase) {
            key.push(c);
        }
    }
    NameKey(KeyText(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn good_titles_and_folders() {
        for title in [
            "Roadmap",
            "meeting 2026-11-07",
            "Ünïcode notes",
            "日本語",
            "a.b",
            "🙂",
        ] {
            assert_eq!(check_segment(title), Ok(()), "{title}");
        }
        for folder in ["", "work", "work/projects", "a/b/c"] {
            assert_eq!(check_folder(folder), Ok(()), "{folder}");
        }
        assert_eq!(check_segment(&"x".repeat(MAX_SEGMENT_BYTES)), Ok(()));
    }

    #[test]
    fn bad_titles() {
        let cases = [
            ("", NameError::Empty),
            (" lead", NameError::BadForm),
            ("trail ", NameError::BadForm),
            (".", NameError::BadForm),
            ("..", NameError::BadForm),
            ("a\u{1b}b", NameError::BadCharacter),
            ("a\u{202E}b", NameError::BadCharacter),
            ("a\tb", NameError::BadCharacter),
        ];
        for (title, err) in cases {
            assert_eq!(check_segment(title), Err(err), "{title:?}");
        }
        for c in INVISIBLE {
            assert_eq!(
                check_segment(&format!("road{c}map")),
                Err(NameError::BadCharacter),
                "{c:?}"
            );
        }
        // Joiners stay: emoji sequences and some scripts need them.
        assert_eq!(check_segment("👨\u{200D}👩\u{200D}👧"), Ok(()));
        assert_eq!(check_segment("می\u{200C}خواهم"), Ok(()));
        for c in RESERVED {
            assert_eq!(
                check_segment(&format!("a{c}b")),
                Err(NameError::BadCharacter),
                "{c}"
            );
        }
        assert_eq!(
            check_segment(&"x".repeat(MAX_SEGMENT_BYTES + 1)),
            Err(NameError::TooLong)
        );
    }

    #[test]
    fn bad_folders() {
        for folder in ["/work", "work/", "a//b", "a/./b", "a/../b", "a/ b"] {
            assert!(check_folder(folder).is_err(), "{folder}");
        }
        let deep = vec!["a"; MAX_FOLDER_LEVELS + 1].join("/");
        assert_eq!(check_folder(&deep), Err(NameError::FolderTooDeep));
        assert_eq!(
            check_folder(&vec!["a"; MAX_FOLDER_LEVELS].join("/")),
            Ok(())
        );
        let long = vec!["x".repeat(200); 6].join("/");
        assert_eq!(check_folder(&long), Err(NameError::FolderTooDeep));
    }

    #[test]
    fn keys_ignore_case() {
        let key = |f, t| unique_key(f, t).as_str().to_owned();
        assert_eq!(key("Work", "Roadmap"), key("work", "ROADMAP"));
        assert!(unique_key("Work", "Roadmap") == unique_key("work", "ROADMAP"));
        assert_ne!(key("work", "roadmap"), key("", "work/roadmap"));
        assert_eq!(key("", "Ünï"), key("", "ünÏ"));
    }

    #[test]
    fn errors_never_show_the_name() {
        for title in ["hunter2 ", "hun/ter2"] {
            let shown = check_segment(title).unwrap_err().to_string();
            assert!(
                !shown.contains("hunter2") && !shown.contains("ter2"),
                "{shown}"
            );
        }
    }

    proptest! {
        // A valid name never holds a character that could break a link,
        // a path or the terminal.
        #[test]
        fn valid_segments_are_safe(s in any::<String>()) {
            if check_segment(&s).is_ok() {
                prop_assert!(!s.contains(|c| RESERVED.contains(&c) || INVISIBLE.contains(&c) || is_forbidden(c)));
                prop_assert!(!s.is_empty() && s.len() <= MAX_SEGMENT_BYTES);
                prop_assert!(s != "." && s != "..");
            }
        }
    }
}
