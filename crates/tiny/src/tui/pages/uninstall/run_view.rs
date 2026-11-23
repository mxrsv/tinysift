//! The screens after the action menu, shaped as Clean's run and done
//! screens (`node tools/dump.js run done`): one line per app with ✓, the
//! spinner or `·`, then the outcome; and the dry-run list of what would go.

use super::engine::UninstallEngine;
use super::model::{figures, label, plural, shown_path, MENU_CONSENT};
use super::run::{Run, Status};
use super::tasks::FrozenApp;
use super::UninstallPage;
use crate::tui::pages::{Detail, KeyHint, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::list_cursor::ListCursor;
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::spinner::spinner_glyph;
use crate::tui::widgets::text_fit::{cut, wrap};

/// Name column of a run line, as Clean's: at most 36 cells.
const RUN_NAME: usize = 36;
const RUN_FIXED: usize = 32;
const SIZE_WIDTH: usize = 10;
/// A dry-run entry: indent, label, size, space.
const ENTRY_FIXED: usize = 4 + 6 + 8 + 1;

/// Rows of the dry-run list: each app, then each entry it would remove.
pub fn dry_run_rows<F>(batch: &[FrozenApp<F>]) -> usize {
    batch
        .iter()
        .map(|app| 1 + app.plan.items_for(MENU_CONSENT).count())
        .sum()
}

fn wrapped(text: &str, width: usize) -> impl Iterator<Item = StyledLine> {
    wrap(text, width)
        .into_iter()
        .map(|line| StyledLine::new().dim(line))
}

impl<E: UninstallEngine> UninstallPage<E> {
    /// While running (`live`) and after: the ✓ list and the outcome.
    pub(super) fn run_view(&self, ctx: &ViewCtx, run: &Run, live: bool) -> PageView {
        let nw = RUN_NAME.min(ctx.list_width.saturating_sub(RUN_FIXED));
        let list = run
            .lines
            .iter()
            .map(|line| {
                let name = format!("{:<nw$}", cut(&line.name, nw));
                let size = |text: String| format!("{text:>SIZE_WIDTH$}");
                match &line.status {
                    Status::Removed => StyledLine::new()
                        .ok("✓ ")
                        .plain(name)
                        .dim(size(format_size(line.bytes))),
                    Status::Running => StyledLine::new()
                        .storage(format!("{} ", spinner_glyph(ctx.tick)))
                        .hi(name)
                        .dim(if run.hard {
                            "  deleting for good…"
                        } else {
                            "  moving to Trash…"
                        }),
                    Status::Waiting => StyledLine::new().faint("· ").dim(cut(&line.name, nw)),
                    Status::Kept(reason) => StyledLine::new()
                        .warn("● ")
                        .plain(name)
                        .dim(size("kept".into()))
                        .dim(format!("  {reason}")),
                    Status::NotRun => StyledLine::new()
                        .faint("· ")
                        .dim(name)
                        .dim(size("not run".into())),
                }
            })
            .collect();
        let (title, full, brief, keys) = if live {
            Self::running_detail(run, ctx)
        } else {
            Self::done_detail(run, ctx)
        };
        PageView {
            list,
            detail: Some(Detail { title, full, brief }),
            keys,
            ..PageView::default()
        }
    }

    fn running_detail(
        run: &Run,
        ctx: &ViewCtx,
    ) -> (String, Vec<StyledLine>, Vec<StyledLine>, Vec<KeyHint>) {
        let done = StyledLine::new()
            .dim("Done so far  ")
            .hi(format_size(run.removed_bytes()));
        let note = if run.stopping {
            StyledLine::new().warn("● ").dim("Stopping after this app.")
        } else {
            StyledLine::new().dim("Each app is checked again right before it moves.")
        };
        let full = std::iter::once(done.clone())
            .chain(std::iter::once(StyledLine::new()))
            .chain(if run.stopping {
                vec![note.clone()]
            } else {
                wrapped(
                    "Each app is checked again right before it moves.",
                    ctx.detail_width,
                )
                .collect()
            })
            .collect();
        (
            "Uninstalling".into(),
            full,
            vec![done, note],
            vec![KeyHint::new("esc", "stop after this app")],
        )
    }

    fn done_detail(
        run: &Run,
        ctx: &ViewCtx,
    ) -> (String, Vec<StyledLine>, Vec<StyledLine>, Vec<KeyHint>) {
        let width = ctx.detail_width;
        let removed = format_size(run.removed_bytes());
        let kept = run.count(|status| matches!(status, Status::Kept(_)));
        let not_run = run.count(|status| *status == Status::NotRun);
        let went = if run.hard {
            StyledLine::new().dim("For good   ").bad(removed.clone())
        } else {
            StyledLine::new()
                .dim("In Trash   ")
                .plain(removed.clone())
                .dim("  Finder › Put Back")
        };
        let counts = [
            (kept > 0).then(|| {
                StyledLine::new()
                    .dim("Kept       ")
                    .warn(plural(kept, "app", "apps"))
            }),
            (not_run > 0).then(|| {
                StyledLine::new()
                    .dim("Not run    ")
                    .plain(plural(not_run, "app", "apps"))
            }),
        ];
        let reasons = run.lines.iter().filter_map(|line| match &line.status {
            Status::Kept(reason) => Some((line.name.as_str(), reason.as_str())),
            _ => None,
        });
        let why: Vec<StyledLine> = reasons
            .flat_map(|(name, reason)| {
                std::iter::once(
                    StyledLine::new()
                        .warn("● ")
                        .hi(cut(name, width.saturating_sub(2))),
                )
                .chain(
                    wrapped(reason, width.saturating_sub(2))
                        .map(|l| StyledLine::from("  ").append(l)),
                )
            })
            .collect();
        let after = if run.refused {
            "Nothing was removed: the engine refused an app before the menu."
        } else if run.hard {
            "Deleted for good. Nothing can restore it."
        } else {
            "Space in the Trash returns when the Trash is emptied."
        };
        // A refusal before the menu ran nothing: no figure to report.
        let outcome = (!run.refused).then(|| {
            [
                StyledLine::new().dim("Removed    ").ok(removed.clone()),
                went,
            ]
        });
        let full = outcome
            .into_iter()
            .flatten()
            .chain(counts.iter().flatten().cloned())
            .chain((!why.is_empty()).then(StyledLine::new))
            .chain(why)
            .chain(
                (kept > 0 && !run.refused)
                    .then(|| wrapped("An app that failed partway may be partly removed.", width))
                    .into_iter()
                    .flatten(),
            )
            .chain(std::iter::once(StyledLine::new()))
            .chain(wrapped(after, width))
            .collect();
        let headline = StyledLine::new()
            .dim("Removed    ")
            .ok(removed.clone())
            .dim(if run.hard {
                "   for good "
            } else {
                "   in Trash "
            })
            .plain(removed)
            .dim(if kept > 0 { "   kept " } else { "" })
            .warn(if kept > 0 {
                plural(kept, "app", "apps")
            } else {
                String::new()
            });
        let second = match run.first_kept() {
            Some((name, reason)) => StyledLine::new()
                .warn("● ")
                .dim(format!("{name}: {reason}")),
            None if run.hard => StyledLine::new().dim("Deleted for good. Nothing can restore it."),
            None => StyledLine::new().dim("Finder › Put Back restores what is in the Trash."),
        };
        let title = if run.refused {
            "Nothing removed"
        } else if not_run > 0 {
            "Stopped"
        } else {
            "Done"
        };
        (
            title.into(),
            full,
            vec![headline, second, StyledLine::new().dim(after)],
            vec![
                KeyHint::new("enter", "back to Uninstall, read again"),
                KeyHint::new("q", "quit"),
            ],
        )
    }

    /// What Move to Trash or Hard delete would remove; nothing changes.
    pub(super) fn dry_run_view<F>(
        &self,
        ctx: &ViewCtx,
        batch: &[FrozenApp<F>],
        cursor: &ListCursor,
    ) -> PageView {
        let library = self.library.as_deref();
        let nw = ctx.list_width.saturating_sub(2 + SIZE_WIDTH);
        let path_width = ctx.list_width.saturating_sub(ENTRY_FIXED);
        let list = batch
            .iter()
            .flat_map(|app| {
                let sums = figures(&app.plan);
                let head = StyledLine::new()
                    .hi(format!("{:<nw$}", cut(&app.plan.app.name, nw)))
                    .dim(format!("{:>SIZE_WIDTH$}", format_size(sums.removable)));
                let entries = app.plan.items_for(MENU_CONSENT).map(|item| {
                    StyledLine::new()
                        .plain("  ")
                        .dim(format!("{:<6}", label(item)))
                        .plain(format!("{:>8}", format_size(item.size)))
                        .plain(" ")
                        .dim(cut(&shown_path(&item.path, library), path_width))
                });
                std::iter::once(head).chain(entries).collect::<Vec<_>>()
            })
            .enumerate()
            .map(|(index, row)| {
                let lead = if index == cursor.index() {
                    StyledLine::new().hi("❯ ")
                } else {
                    StyledLine::from("  ")
                };
                lead.append(row)
            })
            .collect();
        let (bytes, items) = batch.iter().fold((0u64, 0usize), |(bytes, items), app| {
            let sums = figures(&app.plan);
            (bytes.saturating_add(sums.removable), items + sums.items)
        });
        let total = StyledLine::new().dim("Would go   ").hi(format_size(bytes));
        let counts = StyledLine::new().dim(format!(
            "{} · {}",
            plural(batch.len(), "app", "apps"),
            plural(items, "item", "items")
        ));
        let nothing = "Dry-run: nothing was moved or deleted.";
        PageView {
            list,
            cursor: Some(cursor.index()),
            detail: Some(Detail {
                title: "Dry-run".into(),
                full: [total.clone(), counts.clone(), StyledLine::new()]
                    .into_iter()
                    .chain(wrapped(nothing, ctx.detail_width))
                    .collect(),
                brief: vec![total, counts, StyledLine::new().dim(nothing)],
            }),
            keys: [
                KeyHint::new("↑↓", "move"),
                KeyHint::new("enter", "back to the list"),
            ]
            .into_iter()
            .chain(KeyHint::globals())
            .collect(),
            ..PageView::default()
        }
    }
}
