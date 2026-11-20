//! Folded sections in live mode (step LF1; its own file since step RF4):
//! a heading's section hidden behind the heading's line, as in the
//! viewer.
//!
//! Folds are the only state of live mode's look that outlives a frame:
//! the view keeps them as heading line starts (`View::folds`). They are
//! offsets, never text, so every edit moves them (`shift`, from the
//! buffer's `Change`s), and every frame checks them again (`folds`): a
//! heading that is gone, or a section with nothing to hide, drops its
//! fold, and so does the cursor or the selection entering the section,
//! so the cursor is never hidden in a fold.

use std::ops::Range;

use super::look::Editing;
use crate::editor::buffer::Change;

/// A folded section: its heading's line and the lines it hides.
pub struct Fold {
    /// The heading line's start, and where its line break starts.
    pub heading: usize,
    pub end: usize,
    /// From the line after the heading to the next heading of its level
    /// or above, or the text's end.
    pub hidden: Range<usize>,
}

/// The folds kept by heading line start, moved by `change`: one at or
/// after the change moves with it, one inside what it removed is dropped.
pub fn shift(folds: &mut Vec<usize>, change: Change) {
    folds.retain_mut(|fold| {
        if *fold < change.at {
            true
        } else if *fold >= change.at + change.removed {
            *fold = *fold - change.removed + change.inserted;
            true
        } else {
            false
        }
    });
}

/// Step 2, the folds: of the headings the view keeps folded (`kept`, by
/// line start), only one that opens a section and has lines to hide
/// folds; one the cursor or the selection is in opens, for good (it is
/// dropped from `kept`), so the cursor is never in a fold.
pub fn folds(
    kept: &mut Vec<usize>,
    headings: &[(usize, u8)],
    text: &str,
    editing: Editing,
) -> Vec<Fold> {
    let mut folds = Vec::new();
    kept.retain(|&start| match section(headings, text, start) {
        Some(fold) if !entered(&fold, text, editing) => {
            folds.push(fold);
            true
        }
        _ => false,
    });
    folds
}

/// The section of the heading whose line starts at `start`, as it would
/// fold: `None` if no heading starts there, it is the text's last line,
/// or it has no lines to hide.
pub fn section(headings: &[(usize, u8)], text: &str, start: usize) -> Option<Fold> {
    let i = headings
        .binary_search_by_key(&start, |&(line, _)| line)
        .ok()?;
    let level = headings[i].1;
    let newline = start + text[start..].find('\n')?;
    let end = if text[..newline].ends_with('\r') {
        newline - 1
    } else {
        newline
    };
    let section_end = headings[i + 1..]
        .iter()
        .find(|&&(_, l)| l <= level)
        .map_or(text.len(), |&(line, _)| line);
    let hidden = newline + 1..section_end;
    (!hidden.is_empty()).then_some(Fold {
        heading: start,
        end,
        hidden,
    })
}

/// Whether the cursor or the selection is in what `fold` hides (the
/// text's end counts for a section that reaches it), which opens it.
pub fn entered(fold: &Fold, text: &str, editing: Editing) -> bool {
    let hidden = &fold.hidden;
    hidden.contains(&editing.cursor)
        || (hidden.end == text.len() && editing.cursor == text.len())
        || editing.selects(hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Folds move with each change: kept before it, moved after it,
    /// dropped inside what it removed.
    #[test]
    fn folds_move_with_changes() {
        let mut folds = vec![10, 20, 30];
        shift(
            &mut folds,
            Change {
                at: 15,
                removed: 10,
                inserted: 2,
            },
        );
        assert_eq!(folds, [10, 22]);
        // Typed at a heading's start (a line break before it): it moves.
        shift(
            &mut folds,
            Change {
                at: 10,
                removed: 0,
                inserted: 1,
            },
        );
        assert_eq!(folds, [11, 23]);
    }
}
