//! Which link targets the pager may make clickable
//! (docs/decisions/0054-clickable-links.md).
//!
//! A clickable link's target is written into an OSC 8 escape sequence
//! (`ESC ] 8 ; ; TARGET ESC \`), the first note-chosen text Crystal puts
//! inside a sequence rather than into a cell. So the check is an
//! allowlist, and nothing is escaped, repaired or encoded: a target that
//! fails is not clickable and is drawn as text, as before.
//!
//! - **Scheme:** `http://`, `https://` or `mailto:` (any case), with
//!   something after it; for http and https a host, not another `/`.
//!   Other schemes (`file:`, `javascript:`, app handlers) would reach
//!   whatever the desktop has registered for them.
//! - **Characters:** only the printable ASCII RFC 3986 allows in a URI
//!   (letters, digits, `-._~:/?#[]@!$&'()*+,;=%`), so no control
//!   character, space or byte above 0x7E: neither ESC, BEL nor the
//!   sequence's terminator can appear. Every `%` starts a `%XX` escape.
//! - **Length:** at most `MAX_TARGET` bytes, VTE's own limit.

/// The longest target made clickable, in bytes: VTE ignores longer ones.
pub const MAX_TARGET: usize = 2083;

/// Schemes made clickable, lowercase, with what must follow them.
const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];

/// Whether `target`, as the parser decoded it, may be made clickable.
pub fn clickable(target: &str) -> bool {
    let bytes = target.as_bytes();
    if bytes.len() > MAX_TARGET || !bytes.iter().all(|&b| allowed(b)) || !escapes_ok(bytes) {
        return false;
    }
    SCHEMES.iter().any(|scheme| {
        let Some(rest) = bytes
            .get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme.as_bytes()))
            .map(|_| &bytes[scheme.len()..])
        else {
            return false;
        };
        match rest.first() {
            None => false,
            Some(b'/') => !scheme.ends_with("//"),
            Some(_) => true,
        }
    })
}

/// A byte RFC 3986 allows in a URI: unreserved, reserved, or `%`.
fn allowed(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b)
}

/// Every `%` is followed by two hex digits.
fn escapes_ok(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().all(|(i, &b)| {
        b != b'%'
            || bytes
                .get(i + 1..i + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_and_mail_targets_are_clickable() {
        for target in [
            "https://example.org",
            "http://example.org/a/b?x=1&y=2#top",
            "HTTPS://Example.org/Path",
            "https://example.org/a%20b",
            "https://user@example.org:8080/(x);y,z*!$'+=~",
            "https://[::1]/",
            "mailto:me@example.org",
            "mailto:me@example.org?subject=Hi%21",
        ] {
            assert!(clickable(target), "{target}");
        }
    }

    #[test]
    fn other_schemes_are_not() {
        for target in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "steam://run/1",
            "vscode://file/x",
            "ftp://example.org",
            "data:text/html,x",
            "httpx://example.org",
            "example.org",
            "/relative/path",
            "#heading",
            "",
        ] {
            assert!(!clickable(target), "{target}");
        }
    }

    #[test]
    fn empty_or_hostless_targets_are_not() {
        for target in ["https://", "http://", "mailto:", "https:///x", "http:x"] {
            assert!(!clickable(target), "{target}");
        }
    }

    /// Anything that could end or extend the escape sequence, or that a
    /// URI may not hold, makes the target plain text.
    #[test]
    fn hostile_bytes_are_refused() {
        for target in [
            "https://e.org/\u{1b}\\",
            "https://e.org/\u{1b}]8;;https://evil.example\u{7}",
            "https://e.org/\u{7}",
            "https://e.org/\u{9c}",
            "https://e.org/a b",
            "https://e.org/\t",
            "https://e.org/\n",
            "https://e.org/\u{7f}",
            "https://e.org/\"",
            "https://e.org/<x>",
            "https://e.org/\\",
            "https://e.org/^`{|}",
            "https://köln.de",
            "https://e.org/\u{202E}",
            "https://e.org/%",
            "https://e.org/%4",
            "https://e.org/%zz",
        ] {
            assert!(!clickable(target), "{target:?}");
        }
    }

    #[test]
    fn length_is_bounded() {
        let at_most = format!("https://e.org/{}", "a".repeat(MAX_TARGET - 14));
        assert_eq!(at_most.len(), MAX_TARGET);
        assert!(clickable(&at_most));
        assert!(!clickable(&format!("{at_most}a")));
    }

    proptest::proptest! {
        // Whatever is accepted is printable ASCII from the allowlist, under
        // the limit, after an allowed scheme: never a byte that could act
        // on the terminal.
        #[test]
        fn accepted_targets_are_inert(target in proptest::prelude::any::<String>()) {
            let target = format!("https://{target}");
            if clickable(&target) {
                proptest::prop_assert!(target.len() <= MAX_TARGET);
                proptest::prop_assert!(target.bytes().all(|b| (0x21..0x7f).contains(&b)));
                proptest::prop_assert!(target.bytes().all(allowed));
            }
        }

        #[test]
        fn uri_shaped_targets_are_accepted(
            path in "[A-Za-z0-9._~!$&'()*+,;=:@/?#-]{0,60}",
        ) {
            let target = format!("https://example.org/{path}");
            proptest::prop_assert!(clickable(&target));
        }
    }
}
