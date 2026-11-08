//! Rectangles and fixed splits (decision 0031): the screen is a few fixed
//! regions (finder, note pane, backlinks, status line), so no constraint
//! solver. All arithmetic saturates; an area can be empty but never
//! reaches outside its parent.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the viewer loop (Phase 3, V6)")
)]

/// A rectangle of cells: columns `x..x + width`, rows `y..y + height`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    /// Clipped so that `x + width` and `y + height` fit in u16.
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width: if width > u16::MAX - x {
                u16::MAX - x
            } else {
                width
            },
            height: if height > u16::MAX - y {
                u16::MAX - y
            } else {
                height
            },
        }
    }

    /// One past the last column.
    pub const fn right(self) -> u16 {
        self.x + self.width
    }

    /// One past the last row.
    pub const fn bottom(self) -> u16 {
        self.y + self.height
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The part of `self` that is also in `other` (empty if none).
    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right()).max(x);
        let bottom = self.bottom().min(other.bottom()).max(y);
        Self::new(x, y, right - x, bottom - y)
    }

    /// `self` shrunk by `margin` on every side.
    pub fn inner(self, margin: u16) -> Self {
        let twice = margin.saturating_mul(2);
        Self::new(
            self.x.saturating_add(margin).min(self.right()),
            self.y.saturating_add(margin).min(self.bottom()),
            self.width.saturating_sub(twice),
            self.height.saturating_sub(twice),
        )
    }

    /// The first `columns` columns (at most all), and the rest.
    pub fn split_left(self, columns: u16) -> (Self, Self) {
        let left = columns.min(self.width);
        (
            Self::new(self.x, self.y, left, self.height),
            Self::new(self.x + left, self.y, self.width - left, self.height),
        )
    }

    /// The first `rows` rows (at most all), and the rest.
    pub fn split_top(self, rows: u16) -> (Self, Self) {
        let top = rows.min(self.height);
        (
            Self::new(self.x, self.y, self.width, top),
            Self::new(self.x, self.y + top, self.width, self.height - top),
        )
    }

    /// Everything but the last `rows` rows, and those rows.
    pub fn split_bottom(self, rows: u16) -> (Self, Self) {
        self.split_top(self.height.saturating_sub(rows))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn contains(outer: Rect, inner: Rect) -> bool {
        inner.is_empty()
            || (inner.x >= outer.x
                && inner.y >= outer.y
                && inner.right() <= outer.right()
                && inner.bottom() <= outer.bottom())
    }

    #[test]
    fn splits_divide_the_area() {
        let area = Rect::new(2, 1, 80, 24);
        assert_eq!(
            area.split_left(30),
            (Rect::new(2, 1, 30, 24), Rect::new(32, 1, 50, 24))
        );
        assert_eq!(
            area.split_bottom(1),
            (Rect::new(2, 1, 80, 23), Rect::new(2, 24, 80, 1))
        );
        assert_eq!(area.split_top(99).1, Rect::new(2, 25, 80, 0));
        assert_eq!(area.inner(1), Rect::new(3, 2, 78, 22));
        assert!(Rect::new(0, 0, 1, 5).inner(1).is_empty());
    }

    #[test]
    fn new_clips_at_the_end_of_u16() {
        let r = Rect::new(u16::MAX - 1, u16::MAX, 10, 10);
        assert_eq!((r.right(), r.bottom()), (u16::MAX, u16::MAX));
    }

    proptest! {
        #[test]
        fn parts_stay_inside(x: u16, y: u16, w: u16, h: u16, n: u16, m: u16) {
            let area = Rect::new(x, y, w, h);
            let (a, b) = area.split_left(n);
            prop_assert!(contains(area, a) && contains(area, b));
            prop_assert_eq!(u32::from(a.width) + u32::from(b.width), u32::from(area.width));
            let (a, b) = area.split_top(n);
            prop_assert!(contains(area, a) && contains(area, b));
            prop_assert_eq!(u32::from(a.height) + u32::from(b.height), u32::from(area.height));
            let (a, b) = area.split_bottom(n);
            prop_assert!(contains(area, a) && contains(area, b));
            prop_assert!(contains(area, area.inner(n)));
            let other = Rect::new(n, m, w / 2, h / 3);
            let both = area.intersection(other);
            prop_assert!(contains(area, both) && contains(other, both));
        }
    }
}
