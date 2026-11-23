//! The frame's fixed geometry, from the terminal size alone, so no data or
//! state can move a region.

/// Below this the TUI does not open (I7): Terminal.app's default size.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
/// From this width the detail is a right-hand pane (I8).
const TWO_COLUMNS_FROM: usize = 100;
/// The right-hand pane, including its separator column.
const DETAIL_PANE: usize = 44;
/// Tab bar, its rule, the bottom rule and the key bar.
const CHROME_ROWS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    pub two_columns: bool,
    /// Cells in a list row (after the one-cell left margin).
    pub list_width: usize,
    /// Cells in a detail line: the right pane, or the band below the list.
    pub detail_width: usize,
    /// Rows between the two rules.
    pub body_rows: usize,
}

impl Geometry {
    pub fn new(width: u16, height: u16) -> Self {
        let width = usize::from(width);
        let height = usize::from(height);
        let two_columns = width >= TWO_COLUMNS_FROM;
        Self {
            width,
            height,
            two_columns,
            // ' ' + list + ' │ ' + pane: 1 + (W - 47) + 3 + 43 = W
            list_width: if two_columns {
                width - DETAIL_PANE - 3
            } else {
                width.saturating_sub(2)
            },
            detail_width: if two_columns {
                DETAIL_PANE - 1
            } else {
                width.saturating_sub(2)
            },
            body_rows: height.saturating_sub(CHROME_ROWS),
        }
    }

    pub fn fits(width: u16, height: u16) -> bool {
        width >= MIN_WIDTH && height >= MIN_HEIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_mock_at_both_sizes() {
        let wide = Geometry::new(120, 34);
        assert!(wide.two_columns);
        assert_eq!(
            (wide.list_width, wide.detail_width, wide.body_rows),
            (73, 43, 30)
        );
        let std = Geometry::new(80, 24);
        assert!(!std.two_columns);
        assert_eq!(
            (std.list_width, std.detail_width, std.body_rows),
            (78, 78, 20)
        );
        assert!(!Geometry::new(99, 30).two_columns);
        assert!(Geometry::new(100, 30).two_columns);
    }

    #[test]
    fn the_minimum_is_80_by_24() {
        assert!(Geometry::fits(80, 24));
        assert!(!Geometry::fits(79, 24));
        assert!(!Geometry::fits(80, 23));
    }
}
