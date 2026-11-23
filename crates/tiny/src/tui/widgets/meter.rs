//! The meter bar: `━` cells filled in a data colour over a faint track.

use crate::tui::styled_line::{Role, StyledLine};

const CELL: &str = "━";

/// A bar `width` cells wide filled to `fraction` (0 to 1) in `role`. Anything
/// above zero fills at least one cell, so a small figure is never drawn as none.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by Clean, Processes and System rows, plan tasks 05 and 07"
    )
)]
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
