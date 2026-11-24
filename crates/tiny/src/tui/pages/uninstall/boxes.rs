//! The shared action menu and hard-delete box, filled from the frozen plans:
//! the figures are what the chosen action removes, split as the command
//! prints each plan before its menu (`app`, `cache`, `data`), so the consent
//! to remove the user's settings and data is seen before it is given (S1).

use crate::tui::styled_line::{text_width, StyledLine};
use crate::tui::widgets::action_menu::{ActionMenu, MenuChoice};
use crate::tui::widgets::confirm_box::{ConfirmBox, Tone};
use crate::tui::widgets::phrases::{plural, some_names};
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::text_fit::wrap;

use super::model::figures;
use super::tasks::FrozenApp;

/// Names in the hard-delete box: up to three, each cut to this width.
const NAMED: usize = 3;
const NAME_WIDTH: usize = 28;
/// Fact lines stay short of the box's border at 80×24, as the Clean box does.
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

/// What the picked apps' frozen plans remove, added up.
#[derive(Default)]
struct Totals {
    apps: usize,
    bytes: u64,
    items: usize,
    app: u64,
    cache: u64,
    data: u64,
}

fn totals<F>(batch: &[FrozenApp<F>]) -> Totals {
    batch.iter().fold(
        Totals {
            apps: batch.len(),
            ..Totals::default()
        },
        |sum, app| {
            let figures = figures(&app.plan);
            Totals {
                bytes: sum.bytes.saturating_add(figures.removable),
                items: sum.items + figures.items,
                app: sum.app.saturating_add(figures.app),
                cache: sum.cache.saturating_add(figures.cache),
                data: sum.data.saturating_add(figures.data),
                ..sum
            }
        },
    )
}

/// Puts `pieces` on lines of at most `width` cells, `gap` between two on one line.
fn pack(pieces: Vec<StyledLine>, gap: &str, width: usize) -> Vec<StyledLine> {
    let mut lines: Vec<StyledLine> = Vec::new();
    for piece in pieces {
        match lines.last_mut() {
            Some(line) if line.width() + text_width(gap) + piece.width() <= width => {
                *line = std::mem::take(line).dim(gap).append(piece);
            }
            _ => lines.push(piece),
        }
    }
    lines
}

/// `app 48.2 GB · cache 9.1 GB · data 31.0 GB`, then the data named as what
/// it is: the user's settings and data, which the action removes too.
fn split(sum: &Totals) -> Vec<StyledLine> {
    let piece = |label: &str, bytes: u64| {
        StyledLine::new()
            .dim(format!("{label} "))
            .plain(format_size(bytes))
    };
    let parts = pack(
        vec![
            piece("app", sum.app),
            piece("cache", sum.cache),
            piece("data", sum.data),
        ],
        " · ",
        FACT_WIDTH,
    );
    let data = (sum.data > 0).then(|| {
        pack(
            vec![
                StyledLine::new()
                    .warn("● ")
                    .dim("Removes your settings and data too:"),
                StyledLine::new().hi(format_size(sum.data)),
            ],
            " ",
            FACT_WIDTH,
        )
    });
    parts
        .into_iter()
        .chain(data.into_iter().flatten())
        .collect()
}

/// `Uninstall 2 apps · 1.2 GB`, Move to Trash highlighted, the split below.
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
    .with_facts(split(&sum))
}

/// The confirmation Hard delete passes through, Cancel focused.
pub fn hard_delete<F>(batch: &[FrozenApp<F>]) -> ConfirmBox {
    let sum = totals(batch);
    let names: Vec<&str> = batch.iter().map(|app| app.plan.app.name.as_str()).collect();
    let facts = std::iter::once(StyledLine::from(format!(
        "{} · {}",
        format_size(sum.bytes),
        plural(sum.items, "item", "items")
    )))
    .chain(split(&sum))
    .chain(
        wrap(&some_names(&names, NAMED, NAME_WIDTH), FACT_WIDTH)
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
