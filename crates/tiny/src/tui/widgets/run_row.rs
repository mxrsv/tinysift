//! One line of a run, shared by Clean's categories and Uninstall's apps: a
//! mark, the name in a fixed column, then the figure or the state, so a
//! line never moves as its state changes (TU3).

use super::spinner::spinner_glyph;
use super::text_fit::{cut, fill, fill_left};
use crate::tui::styled_line::StyledLine;

/// The name column: at most this many cells, fewer on a narrow list.
const NAME: usize = 36;
/// Cells the rest of a line needs besides the name.
const BESIDE_NAME: usize = 32;
/// The figure column, right-aligned.
const FIGURE: usize = 10;

/// Where one line stands.
pub enum Mark<'a> {
    Waiting,
    /// In progress: the spinner frame and what it is doing (`moving to Trash…`).
    Running {
        tick: u64,
        doing: &'a str,
    },
    /// Finished: the figure it reached, then any `tail` after it.
    Done {
        figure: String,
        tail: StyledLine,
    },
    /// The engine kept it whole: its reason.
    Kept {
        reason: &'a str,
    },
    /// The engine stopped inside it: its reason. It may be partly done.
    Stopped {
        reason: &'a str,
    },
    /// The run ended before it.
    NotRun,
}

/// `✓ name  figure`, `⠹ name  moving to Trash…`, `· name`, and the rest;
/// `icon` is the category's Nerd glyph, or empty.
pub fn run_row(icon: &str, name: &str, mark: Mark<'_>, list_width: usize) -> StyledLine {
    let width = NAME.min(list_width.saturating_sub(BESIDE_NAME));
    let column = fill(name, width);
    let state = |word: &str| fill_left(word, FIGURE);
    match mark {
        Mark::Waiting => StyledLine::new()
            .faint("· ")
            .dim(icon)
            .dim(cut(name, width)),
        Mark::Running { tick, doing } => StyledLine::new()
            .storage(format!("{} ", spinner_glyph(tick)))
            .dim(icon)
            .hi(column)
            .dim(format!("  {doing}")),
        Mark::Done { figure, tail } => StyledLine::new()
            .ok("✓ ")
            .dim(icon)
            .plain(column)
            .dim(fill_left(&figure, FIGURE))
            .append(tail),
        Mark::Kept { reason } => StyledLine::new()
            .warn("● ")
            .dim(icon)
            .plain(column)
            .dim(state("kept"))
            .dim(format!("  {reason}")),
        Mark::Stopped { reason } => StyledLine::new()
            .bad("● ")
            .dim(icon)
            .plain(column)
            .bad(state("stopped"))
            .dim(format!("  {reason}")),
        Mark::NotRun => StyledLine::new()
            .faint("· ")
            .dim(icon)
            .dim(column)
            .dim(state("not run")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_keeps_the_figure_column_in_place() {
        let names = [
            "pnpm store",
            "漢字のキャッシュ",
            "Notes \u{2764}\u{FE0F}",
            "Dev \u{1F469}\u{200D}\u{1F4BB}",
        ];
        let figure_end = 2 + NAME + FIGURE;
        for name in names {
            let done = Mark::Done {
                figure: "1.2 GB".into(),
                tail: StyledLine::new(),
            };
            assert_eq!(run_row("", name, done, 73).width(), figure_end, "{name}");
            assert_eq!(run_row("", name, Mark::NotRun, 73).width(), figure_end);
            let kept = run_row("", name, Mark::Kept { reason: "in use" }, 73);
            assert_eq!(kept.width(), figure_end + 2 + 6, "{name}");
            let stopped = run_row("", name, Mark::Stopped { reason: "failed" }, 73);
            assert_eq!(stopped.width(), figure_end + 2 + 6, "{name}");
        }
    }

    #[test]
    fn the_name_column_narrows_on_a_narrow_list() {
        let line = run_row("", &"x".repeat(80), Mark::NotRun, 60);
        assert_eq!(line.width(), 2 + 28 + 10);
    }
}
