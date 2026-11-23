//! The action box, the hard-delete confirmation and the dry-run plan.

use tiny_core::clean::types::CleanAction;

use crate::tui::pages::{KeyHint, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::action_menu::{ActionMenu, MenuChoice};
use crate::tui::widgets::confirm_box::{ConfirmBox, Tone};
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::text_fit::{middle_ellipsis, wrap};

use super::engine::Route;
use super::listing::Listing;
use super::wording::{plural, shown_path, some_names};

/// The four choices, in the order the menu shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    MoveToTrash,
    DryRun,
    ReviewPaths,
    HardDelete,
}

pub const CHOICES: [Choice; 4] = [
    Choice::MoveToTrash,
    Choice::DryRun,
    Choice::ReviewPaths,
    Choice::HardDelete,
];

/// The warning and the names wrap short of the box's border, as the mock does.
const NAMES_WIDTH: usize = 50;

pub fn action_menu(listing: &Listing) -> ActionMenu {
    let plan = listing.plan(CleanAction::Trash);
    let choice = |label: &str, description: &str, permanent| MenuChoice {
        label: label.into(),
        description: description.into(),
        permanent,
    };
    let menu = ActionMenu::new(
        format!(
            "Clean {} · {}",
            plural(plan.groups.len(), "category", "categories"),
            format_size(plan.bytes())
        ),
        vec![
            choice("Move to Trash", "Put Back works in Finder", false),
            choice("Dry-run", "change nothing, list the paths", false),
            choice("Review paths", "untick single paths first", false),
            choice("Hard delete", "skips the Trash, cannot be undone", true),
        ],
    );
    let tools: Vec<&str> = listing
        .picked()
        .filter(|e| e.category.route == Route::ForGood)
        .map(|e| e.category.label.as_str())
        .collect();
    if tools.is_empty() {
        menu
    } else {
        menu.with_warning(format!(
            "{}: no Trash, removed for good by any choice.",
            some_names(&tools, 2, 20)
        ))
    }
}

/// Names the count, the size, the items, up to three categories and the consequence (TU9).
pub fn hard_delete_box(listing: &Listing) -> ConfirmBox {
    let plan = listing.plan(CleanAction::HardDelete);
    let picked: Vec<&str> = listing
        .picked()
        .map(|e| e.category.label.as_str())
        .collect();
    let names = some_names(&picked, 3, 28);
    let facts = std::iter::once(StyledLine::from(format!(
        "{} · {}",
        format_size(plan.bytes()),
        plural(plan.handed().count(), "item", "items")
    )))
    .chain(
        wrap(&names, NAMES_WIDTH)
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
        format!(
            "Delete {} for good?",
            plural(plan.groups.len(), "category", "categories")
        ),
        "Delete for good",
    )
    .with_facts(facts)
}

/// Dry-run: every path Move to Trash would hand to the engine. Nothing runs.
pub fn dry_run(listing: &Listing, cursor: usize, ctx: &ViewCtx) -> PageView {
    let plan = listing.plan(CleanAction::Trash);
    let width = ctx.list_width.saturating_sub(14);
    let home = listing.home.as_ref();
    let list = plan
        .groups
        .iter()
        .flat_map(|group| {
            let handed: Vec<_> = group
                .items
                .iter()
                .filter(|item| !plan.excluded.contains(&item.path))
                .collect();
            let bytes = handed.iter().fold(0u64, |s, i| s.saturating_add(i.size));
            std::iter::once(StyledLine::new().hi(format!(
                "{} · {} · {}",
                group.label,
                plural(handed.len(), "path", "paths"),
                format_size(bytes)
            )))
            .chain(handed.into_iter().map(move |item| {
                let path = middle_ellipsis(&shown_path(&item.path, home), width);
                StyledLine::new()
                    .plain(format!("  {path:<width$}"))
                    .dim(format!("  {:>10}", format_size(item.size)))
            }))
        })
        .collect::<Vec<_>>();
    let rows = list.len();
    PageView {
        header: vec![
            StyledLine::new().dim("Dry-run: the paths Move to Trash would take. Nothing changes."),
            StyledLine::new(),
        ],
        list,
        cursor: (rows > 0).then_some(cursor.min(rows.saturating_sub(1))),
        foot: Some(StyledLine::new().hi(format_size(plan.bytes())).dim(format!(
            " in {} · {} · nothing was changed",
            plural(plan.groups.len(), "category", "categories"),
            plural(plan.handed().count(), "path", "paths"),
        ))),
        keys: vec![KeyHint::new("↑↓", "move"), KeyHint::new("esc", "back")],
        ..PageView::default()
    }
}

/// Rows in the dry-run list, for its cursor.
pub fn dry_run_rows(listing: &Listing) -> usize {
    let plan = listing.plan(CleanAction::Trash);
    plan.groups.len() + plan.handed().count()
}
