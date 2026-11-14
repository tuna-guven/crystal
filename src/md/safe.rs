//! The sanitizer (docs/design/leakage.md, threat A3;
//! docs/decisions/0037-visible-escapes.md): note text on its way to the
//! screen. Everything that could act on the terminal, or hide or reorder
//! text, is spelled out instead of passed through:
//!
//! - a control character (C0, DEL, C1, which covers ESC and CSI) or a BiDi
//!   control (`ui::grid::is_forbidden`) becomes `<U+XXXX>`, marked so the
//!   renderer draws it dimmed: visible in any font, and it says what was
//!   hidden;
//! - a grapheme that carries an invisible character behind a visible one
//!   (`ui::grid::hides_something`: a tag character, variation selector,
//!   zero-width character or Hangul filler) is spelled out whole, each
//!   character as `<U+XXXX>`, as the editor does, so text that looks the
//!   same is the same; emoji keep their joiners and U+FE0F;
//! - a tab becomes four spaces;
//! - a line ending stays a line ending: `\n`, and `\r\n` becomes `\n`;
//!   a lone `\r` is a control character like any other.
//!
//! `SafeText` can only be made by `sanitize`, so a widget that takes it
//! (L5 on) cannot be handed raw note text. It holds note text: zeroizing,
//! and no Debug, Display, Clone or Serialize.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the renderer and the shell (L4b, L5)")
)]

use std::fmt::Write;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use zeroize::Zeroizing;

use crate::model::assert_not_impl;
use crate::ui::grid::{Mods, Style, hides_something, is_forbidden};
use crate::ui::wrap::Span;

/// The spaces a tab becomes.
const TAB: &str = "    ";
/// `<U+` + up to 6 hex digits + `>`.
const MAX_ESCAPE_LEN: usize = 10;

/// Sanitized note text, and which parts of it are spelled-out characters.
pub struct SafeText {
    text: Zeroizing<String>,
    /// Byte ranges of `text` that are escapes, in order, not overlapping.
    escapes: Vec<Range<usize>>,
}

assert_not_impl!(SafeText: std::fmt::Debug, std::fmt::Display, Clone, serde::Serialize);

/// Sanitizes `raw`. The result is allocated once at its final size, so it
/// never grows (growing would leave unwiped copies behind).
pub fn sanitize(raw: &str) -> SafeText {
    let hidden: usize = raw
        .graphemes(true)
        .map(|g| g.chars().filter(|&c| spelled_out(g, c)).count())
        .sum();
    let tabs = raw.matches('\t').count();
    let capacity = raw.len() + hidden * MAX_ESCAPE_LEN + tabs * TAB.len();
    let mut text = Zeroizing::new(String::with_capacity(capacity));
    let mut escapes = Vec::with_capacity(hidden);
    for grapheme in raw.graphemes(true) {
        match grapheme {
            "\n" | "\r\n" => text.push('\n'),
            "\t" => text.push_str(TAB),
            g => {
                for c in g.chars() {
                    if spelled_out(g, c) {
                        let start = text.len();
                        // Writing to a String cannot fail.
                        let _ = write!(text, "<U+{:04X}>", u32::from(c));
                        escapes.push(start..text.len());
                    } else {
                        text.push(c);
                    }
                }
            }
        }
    }
    debug_assert!(text.len() <= capacity, "the buffer grew");
    SafeText { text, escapes }
}

/// Whether `c`, in `grapheme`, is drawn as `<U+XXXX>`: a forbidden
/// character, or any character of a grapheme that hides one. A line
/// ending and a tab are handled before.
fn spelled_out(grapheme: &str, c: char) -> bool {
    match grapheme {
        "\n" | "\r\n" | "\t" => false,
        _ => is_forbidden(c) || hides_something(grapheme),
    }
}

impl SafeText {
    /// The sanitized text: it holds no forbidden character but `\n`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The spans of `range` in `style`, escapes (or the parts of them in
    /// `range`) dimmed, each with its start offset in the text, appended
    /// to `out`.
    pub fn spans_in<'a>(
        &'a self,
        range: Range<usize>,
        style: Style,
        out: &mut Vec<(usize, Span<'a>)>,
    ) {
        let dim = style.mods(style.mods | Mods::DIM);
        let mut at = range.start;
        let first = self.escapes.partition_point(|e| e.end <= range.start);
        for escape in &self.escapes[first..] {
            if escape.start >= range.end {
                break;
            }
            let (start, end) = (escape.start.max(at), escape.end.min(range.end));
            if start > at {
                out.push((
                    at,
                    Span {
                        text: &self.text[at..start],
                        style,
                    },
                ));
            }
            out.push((
                start,
                Span {
                    text: &self.text[start..end],
                    style: dim,
                },
            ));
            at = end;
        }
        if at < range.end {
            out.push((
                at,
                Span {
                    text: &self.text[at..range.end],
                    style,
                },
            ));
        }
    }

    /// The text as spans in `style`, escapes dimmed, for `ui::wrap`.
    pub fn spans(&self, style: Style) -> Vec<Span<'_>> {
        let dim = style.mods(style.mods | Mods::DIM);
        let mut spans = Vec::with_capacity(self.escapes.len() * 2 + 1);
        let mut at = 0;
        for escape in &self.escapes {
            if escape.start > at {
                spans.push(Span {
                    text: &self.text[at..escape.start],
                    style,
                });
            }
            spans.push(Span {
                text: &self.text[escape.clone()],
                style: dim,
            });
            at = escape.end;
        }
        if at < self.text.len() {
            spans.push(Span {
                text: &self.text[at..],
                style,
            });
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn shown(raw: &str) -> String {
        sanitize(raw).as_str().to_owned()
    }

    #[test]
    fn plain_text_passes_unchanged() {
        for raw in [
            "",
            "hello",
            "Ünïcode 日本語 🙂",
            "a\nb",
            "e\u{301}",
            // Emoji keep their joiners and U+FE0F.
            "👨\u{200D}💻 ❤\u{FE0F} 1\u{FE0F}\u{20E3}",
        ] {
            assert_eq!(shown(raw), raw, "{raw:?}");
            assert!(sanitize(raw).escapes.is_empty());
        }
    }

    #[test]
    fn hidden_characters_are_spelled_out() {
        let cases = [
            ("\u{1b}[2J", "<U+001B>[2J"),
            ("a\u{7}b", "a<U+0007>b"),
            ("\u{9b}31m", "<U+009B>31m"),
            ("\u{7f}", "<U+007F>"),
            ("\u{0}", "<U+0000>"),
            ("abc\u{202E}fed", "abc<U+202E>fed"),
            ("\u{2066}x\u{2069}", "<U+2066>x<U+2069>"),
            ("\u{200F}\u{061C}", "<U+200F><U+061C>"),
            ("lone\rcr", "lone<U+000D>cr"),
            // Invisible characters: alone, or spelled out with the visible
            // character they ride on (decision 0037).
            ("zero\u{200D}width", "zer<U+006F><U+200D>width"),
            ("a\u{E0041}b", "<U+0061><U+E0041>b"),
            ("x\u{200B}y", "x<U+200B>y"),
            ("a\u{3164}b", "a<U+3164>b"),
            ("😀\u{E0101}", "<U+1F600><U+E0101>"),
        ];
        for (raw, want) in cases {
            assert_eq!(shown(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn tabs_and_line_endings() {
        assert_eq!(shown("a\tb"), "a    b");
        assert_eq!(shown("a\r\nb\nc"), "a\nb\nc");
        assert_eq!(shown("\r\n\r\n"), "\n\n");
        assert_eq!(shown("a\r"), "a<U+000D>");
    }

    #[test]
    fn spans_dim_the_escapes() {
        let safe = sanitize("x\u{1b}y\u{202E}");
        let spans = safe.spans(Style::PLAIN);
        let parts: Vec<(&str, bool)> = spans
            .iter()
            .map(|s| (s.text, s.style.mods.contains(Mods::DIM)))
            .collect();
        assert_eq!(
            parts,
            [
                ("x", false),
                ("<U+001B>", true),
                ("y", false),
                ("<U+202E>", true)
            ]
        );
        assert!(sanitize("").spans(Style::PLAIN).is_empty());
    }

    #[test]
    fn spans_in_a_range_cut_escapes() {
        let safe = sanitize("ab\u{1b}cd");
        // "ab<U+001B>cd": the escape is 2..10.
        let mut out = Vec::new();
        safe.spans_in(1..5, Style::PLAIN, &mut out);
        let parts: Vec<(usize, &str, bool)> = out
            .iter()
            .map(|(at, s)| (*at, s.text, s.style.mods.contains(Mods::DIM)))
            .collect();
        assert_eq!(parts, [(1, "b", false), (2, "<U+", true)]);
        out.clear();
        safe.spans_in(8..12, Style::PLAIN, &mut out);
        let parts: Vec<(usize, &str)> = out.iter().map(|(at, s)| (*at, s.text)).collect();
        assert_eq!(parts, [(8, "B>"), (10, "cd")]);
    }

    proptest! {
        // The output never holds a forbidden character but the newline,
        // and the escapes are exactly the dimmed spans.
        #[test]
        fn output_is_always_safe(raw in any::<String>()) {
            let safe = sanitize(&raw);
            prop_assert!(!safe.as_str().contains(|c| c != '\n' && is_forbidden(c)));
            prop_assert!(safe.text.capacity() >= safe.text.len());
            let joined: String = safe.spans(Style::PLAIN).iter().map(|s| s.text).collect();
            prop_assert_eq!(joined.as_str(), safe.as_str());
            for e in &safe.escapes {
                let escape = &safe.as_str()[e.clone()];
                prop_assert!(escape.starts_with("<U+") && escape.ends_with('>'));
            }
        }

        // Text without hidden characters, tabs or carriage returns is not
        // changed at all.
        #[test]
        fn safe_text_is_untouched(raw in "[^\\p{Cc}\\u{061C}\\u{200E}\\u{200F}\\u{202A}-\\u{202E}\\u{2066}-\\u{2069}]*") {
            prop_assume!(!raw.graphemes(true).any(hides_something));
            prop_assert_eq!(shown(&raw), raw);
        }

        // No invisible character reaches the output, but a joiner or
        // U+FE0F inside an emoji.
        #[test]
        fn invisible_characters_are_spelled_out(
            raw in "[ae\u{301}\u{E0041}\u{200B}\u{200D}\u{FE01}\u{FE0F}\u{3164}\u{2060}1\u{20E3}😀👨💻\n]{0,40}",
        ) {
            let safe = sanitize(&raw);
            for grapheme in safe.as_str().graphemes(true) {
                prop_assert!(!hides_something(grapheme), "{:?}", grapheme);
            }
        }

        // The buffer is sized up front and never grows.
        #[test]
        fn never_grows(raw in any::<String>()) {
            let hidden: usize = raw
                .graphemes(true)
                .filter(|&g| g != "\n" && g != "\r\n" && g != "\t")
                .map(|g| if hides_something(g) {
                    g.chars().count()
                } else {
                    g.chars().filter(|&c| is_forbidden(c)).count()
                })
                .sum();
            let tabs = raw.matches('\t').count();
            let capacity = raw.len() + hidden * MAX_ESCAPE_LEN + tabs * TAB.len();
            prop_assert!(sanitize(&raw).text.len() <= capacity);
        }
    }
}
