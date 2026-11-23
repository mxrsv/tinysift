//! The centred box drawn over a page: `╭─ Title ───╮`, rows, `╰───╯`.

use crate::tui::styled_line::StyledLine;

/// Widest box the frame draws; narrower terminals get `width - 6`.
pub const MAX_BOX_WIDTH: usize = 58;
const SIDE_MARGIN: usize = 6;

/// A box over the page. `lines` are the rows inside it, drawn one cell in
/// from the left border; blank lines are spacing. The frame centres it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub title: String,
    pub lines: Vec<StyledLine>,
}

impl Overlay {
    /// The box width on a terminal `terminal_width` cells wide.
    pub fn width_on(terminal_width: usize) -> usize {
        terminal_width
            .saturating_sub(SIDE_MARGIN)
            .min(MAX_BOX_WIDTH)
    }

    /// Cells inside the borders, after the one-cell left margin: the width
    /// a line can fill without being cut.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used by the action menu and confirmation box, plan task 07"
        )
    )]
    pub fn content_width(box_width: usize) -> usize {
        box_width.saturating_sub(3)
    }

    /// The box as rows exactly `width` cells wide.
    pub fn boxed(&self, width: usize) -> Vec<StyledLine> {
        let inner = width.saturating_sub(2);
        let title = crate::tui::widgets::text_fit::cut(&self.title, width.saturating_sub(6));
        let rule = width.saturating_sub(crate::tui::styled_line::text_width(&title) + 5);
        let top = StyledLine::new()
            .faint("╭─ ")
            .hi(title)
            .faint(format!(" {}╮", "─".repeat(rule)));
        let bottom = StyledLine::new().faint(format!("╰{}╯", "─".repeat(inner)));
        std::iter::once(top.fit(width))
            .chain(self.lines.iter().map(|line| {
                StyledLine::new()
                    .faint("│")
                    .append(StyledLine::from(" ").append(line.clone()).fit(inner))
                    .faint("│")
            }))
            .chain(std::iter::once(bottom.fit(width)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_the_title_in_the_top_border_at_the_exact_width() {
        let overlay = Overlay {
            title: "Hard delete".into(),
            lines: vec![
                StyledLine::new(),
                StyledLine::from("Delete 5 categories for good?"),
            ],
        };
        let rows: Vec<String> = overlay.boxed(24).iter().map(StyledLine::text).collect();
        assert_eq!(
            rows,
            [
                "╭─ Hard delete ────────╮",
                "│                      │",
                "│ Delete 5 categories …│",
                "╰──────────────────────╯",
            ]
        );
    }

    #[test]
    fn the_box_is_58_wide_or_six_less_than_the_terminal() {
        assert_eq!(Overlay::width_on(120), 58);
        assert_eq!(Overlay::width_on(80), 58);
        assert_eq!(Overlay::width_on(60), 54);
        assert_eq!(Overlay::content_width(58), 55);
    }
}
