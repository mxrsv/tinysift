//! The centred box drawn over a page: `╭─ Title ───╮`, rows, `╰───╯`.

use crate::tui::styled_line::StyledLine;

/// Widest box the frame draws; narrower terminals get `width - 2`, so from
/// the 60-column minimum every box keeps the width its lines were written for.
pub const MAX_BOX_WIDTH: usize = 58;
const SIDE_MARGIN: usize = 2;

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

    /// The box at most `max_rows` tall: when it would be taller, blank
    /// spacing lines go first, from the bottom up, so no content row is cut.
    pub fn boxed_within(&self, width: usize, max_rows: usize) -> Vec<StyledLine> {
        let over = (self.lines.len() + 2).saturating_sub(max_rows);
        let blanks: Vec<usize> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.width() == 0)
            .map(|(index, _)| index)
            .rev()
            .take(over)
            .collect();
        Self {
            title: self.title.clone(),
            lines: self
                .lines
                .iter()
                .enumerate()
                .filter(|(index, _)| !blanks.contains(index))
                .map(|(_, line)| line.clone())
                .collect(),
        }
        .boxed(width)
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
    fn a_short_window_drops_blank_lines_before_any_content() {
        let overlay = Overlay {
            title: "Keys".into(),
            lines: vec![
                StyledLine::new(),
                StyledLine::from("a"),
                StyledLine::new(),
                StyledLine::from("b"),
                StyledLine::new(),
            ],
        };
        let rows = |max| -> Vec<String> {
            overlay
                .boxed_within(12, max)
                .iter()
                .map(StyledLine::text)
                .collect()
        };
        assert_eq!(rows(10).len(), 7);
        assert_eq!(
            rows(5)[1..4],
            ["│          │", "│ a        │", "│ b        │"]
        );
        assert_eq!(rows(4)[1..3], ["│ a        │", "│ b        │"]);
    }

    #[test]
    fn the_box_is_58_wide_or_two_less_than_the_terminal() {
        assert_eq!(Overlay::width_on(120), 58);
        assert_eq!(Overlay::width_on(80), 58);
        assert_eq!(Overlay::width_on(60), 58);
        assert_eq!(Overlay::width_on(50), 48);
    }
}
