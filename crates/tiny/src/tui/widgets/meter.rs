//! The meter bar: `━` cells filled in a data colour over a faint track.

use crate::tui::styled_line::{Role, StyledLine};

const CELL: &str = "━";

/// The bar's width beside a name column.
pub const BAR: usize = 12;
/// A narrow window shortens the bar, never below this, before a name gets
/// fewer than [`NAME_FLOOR`] cells.
const BAR_FLOOR: usize = 4;
const NAME_FLOOR: usize = 24;

/// Splits `room` cells between a name and its bar: the bar is [`BAR`] wide
/// while the name keeps [`NAME_FLOOR`] cells, then gives way down to
/// [`BAR_FLOOR`]. Returns `(name, bar)`.
pub fn name_and_bar(room: usize) -> (usize, usize) {
    let bar = room
        .saturating_sub(NAME_FLOOR)
        .clamp(BAR_FLOOR, BAR)
        .min(room);
    (room.saturating_sub(bar), bar)
}

/// A bar `width` cells wide filled to `fraction` (0 to 1) in `role`. Anything
/// above zero fills at least one cell, so a small figure is never drawn as none.
pub fn meter(fraction: f64, width: usize, role: Role) -> StyledLine {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let rounded = (fraction * width as f64).round() as usize;
    let filled = rounded.max(usize::from(fraction > 0.0)).min(width);
    StyledLine::new()
        .push(role, CELL.repeat(filled))
        .faint(CELL.repeat(width - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(line: &StyledLine) -> usize {
        line.segs()
            .iter()
            .filter(|seg| seg.role != Role::Faint)
            .map(|seg| seg.text.chars().count())
            .sum()
    }

    #[test]
    fn a_narrow_row_shortens_the_bar_before_the_name() {
        assert_eq!(name_and_bar(60), (48, 12));
        assert_eq!(name_and_bar(36), (24, 12));
        assert_eq!(name_and_bar(31), (24, 7));
        assert_eq!(name_and_bar(20), (16, 4));
        assert_eq!(name_and_bar(2), (0, 2));
    }

    #[test]
    fn fills_in_proportion_and_keeps_the_width() {
        let bar = meter(0.5, 12, Role::Storage);
        assert_eq!(bar.width(), 12);
        assert_eq!(filled(&bar), 6);
        assert_eq!(bar.segs()[0].role, Role::Storage);
    }

    #[test]
    fn a_tiny_figure_still_shows_one_cell_and_zero_shows_none() {
        assert_eq!(filled(&meter(0.0001, 12, Role::Cpu)), 1);
        assert_eq!(filled(&meter(0.0, 12, Role::Cpu)), 0);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        assert_eq!(filled(&meter(11.9, 12, Role::Cpu)), 12);
        assert_eq!(filled(&meter(-1.0, 12, Role::Cpu)), 0);
        assert_eq!(filled(&meter(f64::NAN, 12, Role::Cpu)), 0);
        assert_eq!(meter(0.5, 0, Role::Cpu).width(), 0);
    }
}
