//! The shared action menu and hard-delete box, filled from the frozen plans:
//! the figures are what the chosen action removes.

use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::action_menu::{ActionMenu, MenuChoice};
use crate::tui::widgets::confirm_box::{ConfirmBox, Tone};
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::text_fit::{cut, wrap};

use super::model::{figures, plural};
use super::tasks::FrozenApp;

/// Names in the hard-delete box: up to three, each cut to this width.
const NAMED: usize = 3;
const NAME_WIDTH: usize = 28;
/// The names wrap short of the box's border, as the Clean box does.
const FACT_WIDTH: usize = 50;

/// The command's own choices, in its order; Cancel is esc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Trash,
    DryRun,
    Hard,
}

impl Choice {
    const ALL: [Choice; 3] = [Choice::Trash, Choice::DryRun, Choice::Hard];

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    fn entry(self) -> MenuChoice {
        let (label, description, permanent) = match self {
            Self::Trash => ("Move to Trash", "Put Back works in Finder", false),
            Self::DryRun => ("Dry-run", "change nothing, list what would go", false),
            Self::Hard => ("Hard delete", "skips the Trash, cannot be undone", true),
        };
        MenuChoice {
            label: label.into(),
            description: description.into(),
            permanent,
        }
    }
}

struct Totals {
    apps: usize,
    bytes: u64,
    items: usize,
}

fn totals<F>(batch: &[FrozenApp<F>]) -> Totals {
    batch.iter().fold(
        Totals {
            apps: batch.len(),
            bytes: 0,
            items: 0,
        },
        |sum, app| {
            let figures = figures(&app.plan);
            Totals {
                bytes: sum.bytes.saturating_add(figures.removable),
                items: sum.items + figures.items,
                ..sum
            }
        },
    )
}

/// `Uninstall 2 apps · 1.2 GB`, Move to Trash highlighted.
pub fn action_menu<F>(batch: &[FrozenApp<F>]) -> ActionMenu {
    let sum = totals(batch);
    ActionMenu::new(
        format!(
            "Uninstall {} · {}",
            plural(sum.apps, "app", "apps"),
            format_size(sum.bytes)
        ),
        Choice::ALL.iter().map(|choice| choice.entry()).collect(),
    )
}

/// The confirmation Hard delete passes through, Cancel focused.
pub fn hard_delete<F>(batch: &[FrozenApp<F>]) -> ConfirmBox {
    let sum = totals(batch);
    let names = batch
        .iter()
        .take(NAMED)
        .map(|app| cut(&app.plan.app.name, NAME_WIDTH))
        .collect::<Vec<_>>()
        .join(", ");
    let more = batch.len().saturating_sub(NAMED);
    let names = if more > 0 {
        format!("{names} and {more} more")
    } else {
        names
    };
    let facts = std::iter::once(StyledLine::from(format!(
        "{} · {}",
        format_size(sum.bytes),
        plural(sum.items, "item", "items")
    )))
    .chain(
        wrap(&names, FACT_WIDTH)
            .into_iter()
            .map(|line| StyledLine::new().dim(line)),
    )
    .chain(std::iter::once(
        StyledLine::new().dim("It skips the Trash. Nothing can restore it."),
    ))
    .collect();
    ConfirmBox::new(
        "Hard delete",
        Tone::Permanent,
        format!("Delete {} for good?", plural(sum.apps, "app", "apps")),
        "Delete for good",
    )
    .with_facts(facts)
}
