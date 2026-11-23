//! The running ✓ list (TU3) and the done screen (TU11).

use std::time::Duration;

use tiny_core::clean::types::CleanAction;

use crate::tui::pages::{Detail, KeyHint, PageView, ViewCtx};
use crate::tui::styled_line::{Role, StyledLine};
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::spinner::spinner_glyph;
use crate::tui::widgets::text_fit::{cut, middle_ellipsis, wrap};

use super::run::{RowState, Run, RunRow};
use super::wording::{plural, seconds, shown_path};

/// Names are cut here so the size column stays put on wide terminals.
const MAX_NAME: usize = 36;
const TRASH_RETURNS: &str = "Space in the Trash returns when the Trash is emptied.";

fn name_width(ctx: &ViewCtx) -> usize {
    MAX_NAME.min(ctx.list_width.saturating_sub(32))
}

pub fn running(run: &Run, elapsed: Duration, ctx: &ViewCtx) -> PageView {
    let current = run.running();
    let list = run
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            if Some(i) == current {
                running_row(run, row, ctx)
            } else {
                row_line(row, ctx)
            }
        })
        .collect();
    let width = ctx.detail_width;
    let full = [
        StyledLine::new()
            .dim("Done so far  ")
            .hi(format_size(run.totals().cleaned)),
        StyledLine::new()
            .dim("Elapsed      ")
            .plain(seconds(elapsed)),
        StyledLine::new(),
    ]
    .into_iter()
    .chain(
        wrap(
            "Each category is checked again right before it moves.",
            width,
        )
        .into_iter()
        .map(|l| StyledLine::new().dim(l)),
    )
    .chain(run.stopping.then(|| {
        StyledLine::new()
            .warn("● ")
            .dim("Stops after this category.")
    }))
    .collect();
    let keys = if run.stopping {
        vec![KeyHint::new("stopping", "after this category")]
    } else {
        vec![KeyHint::new("esc", "stop after this category")]
    };
    PageView {
        list,
        detail: Some(Detail {
            title: "Cleaning".into(),
            full,
            brief: Vec::new(),
        }),
        keys,
        ..PageView::default()
    }
}

fn running_row(run: &Run, row: &RunRow, ctx: &ViewCtx) -> StyledLine {
    let width = name_width(ctx);
    let doing = if row.for_good || run.action == CleanAction::HardDelete {
        "removing for good…"
    } else {
        "moving to Trash…"
    };
    StyledLine::new()
        .push(Role::Storage, format!("{} ", spinner_glyph(ctx.tick)))
        .dim(ctx.icons.category_prefix(&row.id))
        .hi(format!("{:<width$}", cut(&row.label, width)))
        .dim(format!("  {doing}"))
}

fn row_line(row: &RunRow, ctx: &ViewCtx) -> StyledLine {
    let width = name_width(ctx);
    let icon = ctx.icons.category_prefix(&row.id);
    let name = format!("{:<width$}", cut(&row.label, width));
    match &row.state {
        RowState::Waiting => StyledLine::new()
            .faint("· ")
            .dim(icon)
            .dim(cut(&row.label, width)),
        RowState::NotRun => StyledLine::new()
            .faint("· ")
            .dim(icon)
            .dim(name)
            .dim(format!("{:>10}", "not run")),
        RowState::Finished(moved) if moved.whole_kept => {
            let reason = moved.kept.first().map_or("", |(_, r)| r.as_str());
            StyledLine::new()
                .warn("● ")
                .dim(icon)
                .plain(name)
                .dim(format!("{:>10}", "kept"))
                .dim(format!("  {reason}"))
        }
        RowState::Finished(moved) => {
            let line = StyledLine::new()
                .ok("✓ ")
                .dim(icon)
                .plain(name)
                .dim(format!("{:>10}", format_size(moved.bytes)));
            let line = if moved.kept.is_empty() {
                line
            } else {
                line.warn(format!(
                    "  {} kept",
                    plural(moved.kept.len(), "path", "paths")
                ))
            };
            if moved.requested {
                line.dim("  requested")
            } else {
                line
            }
        }
    }
}

pub fn done(run: &Run, home: Option<&std::path::PathBuf>, ctx: &ViewCtx) -> PageView {
    let totals = run.totals();
    let width = ctx.detail_width;
    let list = run.rows.iter().map(|row| row_line(row, ctx)).collect();
    let not_run = run
        .rows
        .iter()
        .filter(|r| r.state == RowState::NotRun)
        .count();
    let part = run.partly_kept();
    let mut full = vec![
        StyledLine::new()
            .dim("Cleaned    ")
            .ok(format_size(totals.cleaned)),
        StyledLine::new()
            .dim("In Trash   ")
            .plain(format_size(totals.in_trash))
            .dim("  Finder › Put Back"),
    ];
    if totals.for_good > 0 {
        full.push(
            StyledLine::new()
                .dim("For good   ")
                .bad(format_size(totals.for_good)),
        );
    }
    if totals.kept > 0 {
        full.push(
            StyledLine::new()
                .dim("Kept       ")
                .warn(format_size(totals.kept)),
        );
    }
    if not_run > 0 {
        full.push(StyledLine::new().dim("Not run    ").plain(plural(
            not_run,
            "category",
            "categories",
        )));
    }
    if let Some((_, moved)) = part {
        let (path, reason) = &moved.kept[0];
        full.extend([
            StyledLine::new(),
            StyledLine::new().dim(middle_ellipsis(&shown_path(path, home), width)),
            StyledLine::new()
                .warn("● ")
                .dim(cut(reason, width.saturating_sub(2))),
        ]);
    }
    if let Some(error) = &run.error {
        full.push(StyledLine::new());
        full.extend(
            wrap(&format!("Stopped: {error}"), width.saturating_sub(2))
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    let lead = if i == 0 { "● " } else { "  " };
                    StyledLine::new().bad(lead).dim(l)
                }),
        );
    }
    full.push(StyledLine::new());
    full.extend(
        wrap(TRASH_RETURNS, width)
            .into_iter()
            .map(|l| StyledLine::new().dim(l)),
    );

    let summary = StyledLine::new()
        .dim("Cleaned    ")
        .ok(format_size(totals.cleaned))
        .dim("   in Trash ")
        .plain(format_size(totals.in_trash));
    let summary = if totals.for_good > 0 {
        summary
            .dim("   for good ")
            .bad(format_size(totals.for_good))
    } else {
        summary
    };
    let summary = if totals.kept > 0 {
        summary.dim("   kept ").warn(format_size(totals.kept))
    } else {
        summary
    };
    let second = if let Some(error) = &run.error {
        StyledLine::new().bad("● ").dim(format!("Stopped: {error}"))
    } else if let Some((_, moved)) = part {
        StyledLine::new().warn("● ").dim(format!(
            "{} kept: {}",
            plural(moved.kept.len(), "path", "paths"),
            moved.kept[0].1
        ))
    } else if not_run > 0 {
        StyledLine::new().dim(format!(
            "Stopped: {} not run.",
            plural(not_run, "category", "categories")
        ))
    } else {
        StyledLine::new().dim("Finder › Put Back restores what is in the Trash.")
    };
    PageView {
        list,
        detail: Some(Detail {
            title: format!("Done in {}", seconds(run.took.unwrap_or_default())),
            full,
            brief: vec![summary, second, StyledLine::new().dim(TRASH_RETURNS)],
        }),
        keys: vec![
            KeyHint::new("enter", "back to Clean, measured again"),
            KeyHint::new("q", "quit"),
        ],
        ..PageView::default()
    }
}
