//! The list of apps, the highlighted app's detail and the foot: the screens
//! of `node tools/dump.js uninstall` and its reading, empty and error lines.

use tiny_core::uninstall::{valid_bundle_id, AppEntry, Plan};

use super::engine::UninstallEngine;
use super::model::{
    figures, kept_place, kept_reason, label, last_used, short_refusal, shown_path, AppRow,
};
use super::{PlanRead, UninstallPage};
use crate::tui::pages::{Detail, KeyHint, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::phrases::plural;
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::spinner::spinner_line;
use crate::tui::widgets::text_fit::{cut, fill, wrap};

/// Cells a row spends besides the name: cursor, tick, size, gap, last used, gap.
const ROW_FIXED: usize = 37;
const SIZE_WIDTH: usize = 10;
const LAST_WIDTH: usize = 10;
/// An entry line: `data  ` then the size in 8 cells and a space.
const ENTRY_LABEL: usize = 6;
const ENTRY_SIZE: usize = 8;
/// The brief's first line leaves this much for the bundle identifier.
const BRIEF_ID_ROOM: usize = 34;

/// What the detail knows of the highlighted app's plan.
enum Known<'a> {
    Refused(&'a str),
    Measuring,
    Failed(&'a str),
    Ready(&'a Plan),
}

fn name_width(ctx: &ViewCtx) -> usize {
    ctx.list_width.saturating_sub(ROW_FIXED)
}

fn column_header(ctx: &ViewCtx) -> StyledLine {
    let nw = name_width(ctx);
    StyledLine::new().dim(format!(
        "    {:<nw$}{:>SIZE_WIDTH$}  {:<LAST_WIDTH$}",
        "NAME", "SIZE", "LAST USED"
    ))
}

fn spinner(ctx: &ViewCtx, text: &str) -> StyledLine {
    spinner_line(ctx.tick, text)
}

/// The bundle identifier, or why leftovers were not looked up, as the command says.
fn identity(app: &AppEntry) -> String {
    match app.bundle_id.as_deref() {
        Some(id) if valid_bundle_id(id) => id.into(),
        Some(id) => format!("{id} · not a valid identifier, leftovers not searched"),
        None => "bundle identifier unknown · leftovers not searched".into(),
    }
}

impl<E: UninstallEngine> UninstallPage<E> {
    pub(super) fn reading_view(&self, ctx: &ViewCtx) -> PageView {
        PageView {
            header: vec![column_header(ctx)],
            list: vec![spinner(ctx, "reading /Applications…")],
            keys: [KeyHint::new("esc", "stop")]
                .into_iter()
                .chain(KeyHint::globals())
                .collect(),
            ..PageView::default()
        }
    }

    /// The read stopped (`reason` is `None`) or failed: one line and how to go on.
    pub(super) fn notice_view(&self, ctx: &ViewCtx, reason: Option<&String>) -> PageView {
        let list = match reason {
            Some(reason) => [
                StyledLine::new(),
                StyledLine::new()
                    .bad("● ")
                    .hi("/Applications could not be read"),
            ]
            .into_iter()
            .chain(
                wrap(reason, ctx.list_width.saturating_sub(2))
                    .into_iter()
                    .map(|line| StyledLine::new().dim(format!("  {line}"))),
            )
            .collect(),
            None => vec![
                StyledLine::new(),
                StyledLine::new()
                    .warn("● ")
                    .hi("Reading /Applications stopped"),
                StyledLine::new()
                    .dim("  Nothing is listed until it is read. Press r to read again."),
            ],
        };
        Self::still_view(list)
    }

    pub(super) fn empty_view(&self, _ctx: &ViewCtx) -> PageView {
        Self::still_view(vec![
            StyledLine::new(),
            StyledLine::new().ok("✓ ").hi("No apps in /Applications"),
            StyledLine::new().dim("  Nothing to uninstall here."),
        ])
    }

    fn still_view(list: Vec<StyledLine>) -> PageView {
        PageView {
            list,
            keys: [KeyHint::new("r", "read again")]
                .into_iter()
                .chain(KeyHint::globals())
                .collect(),
            ..PageView::default()
        }
    }

    pub(super) fn browse_view(&self, ctx: &ViewCtx) -> PageView {
        let keys = [
            KeyHint::new("↑↓", "move"),
            KeyHint::new("space", "tick"),
            KeyHint::new("s", "sort"),
            KeyHint::new("enter", "uninstall"),
        ];
        PageView {
            header: vec![column_header(ctx)],
            list: self
                .apps
                .iter()
                .enumerate()
                .map(|(index, row)| self.row(index, row, ctx))
                .collect(),
            cursor: Some(self.cursor.index()),
            detail: self.highlighted().map(|row| self.detail(row, ctx)),
            foot: Some(self.foot()),
            keys: keys.into_iter().chain(KeyHint::globals()).collect(),
            ..PageView::default()
        }
    }

    /// The list while the picked plans are frozen: the foot says so.
    pub(super) fn freezing_view(&self, ctx: &ViewCtx) -> PageView {
        let count = plural(self.picked().len(), "app", "apps");
        PageView {
            foot: Some(spinner(
                ctx,
                &format!("checking {count} before the menu opens…"),
            )),
            keys: [KeyHint::new("esc", "cancel")]
                .into_iter()
                .chain(KeyHint::globals())
                .collect(),
            ..self.browse_view(ctx)
        }
    }

    fn row(&self, index: usize, row: &AppRow, ctx: &ViewCtx) -> StyledLine {
        let nw = name_width(ctx);
        let name = fill(&cut(&row.app.name, nw.saturating_sub(1)), nw);
        let ticked = self.ticked.contains(&row.app.path);
        let lead = if index == self.cursor.index() {
            StyledLine::new().hi("❯ ")
        } else {
            StyledLine::from("  ")
        };
        let line = match (&row.refusal, ticked) {
            (Some(_), _) => lead.faint("– ").dim(name),
            (None, true) => lead.hi("◉ ").hi(name),
            (None, false) => lead.faint("◯ ").plain(name),
        };
        let line = line
            .dim(format!("{:>SIZE_WIDTH$}", format_size(row.app.size)))
            .plain("  ")
            .dim(format!(
                "{:<LAST_WIDTH$}",
                last_used(row.app.last_used_days)
            ));
        match &row.refusal {
            Some(reason) => line.plain(" ").dim(short_refusal(reason)),
            None => line,
        }
    }

    fn known<'a>(&'a self, row: &'a AppRow) -> Known<'a> {
        if let Some(reason) = &row.refusal {
            return Known::Refused(reason);
        }
        match self.plans.get(&row.app.path) {
            Some(PlanRead::Ready(plan)) => match &plan.blocked {
                Some(reason) => Known::Refused(reason),
                None => Known::Ready(plan),
            },
            Some(PlanRead::Failed(reason)) => Known::Failed(reason),
            None => Known::Measuring,
        }
    }

    fn detail(&self, row: &AppRow, ctx: &ViewCtx) -> Detail {
        let width = ctx.detail_width;
        let app = &row.app;
        let known = self.known(row);
        let full = wrap(&app.name, width)
            .into_iter()
            .map(|line| StyledLine::new().hi(line))
            .chain([
                StyledLine::new().dim(cut(&identity(app), width)),
                StyledLine::new(),
            ])
            .chain(self.known_lines(&known, ctx))
            .collect();
        let head = StyledLine::new()
            .hi(cut(&app.name, width.saturating_sub(BRIEF_ID_ROOM)))
            .dim(format!("  {}", identity(app)));
        let brief = std::iter::once(head)
            .chain(Self::known_brief(&known, ctx))
            .collect();
        Detail {
            title: "Highlighted".into(),
            full,
            brief,
        }
    }

    /// The right pane under the name: one line per entry, then Removable.
    fn known_lines(&self, known: &Known<'_>, ctx: &ViewCtx) -> Vec<StyledLine> {
        let width = ctx.detail_width;
        let reason_lines = |reason: &str| -> Vec<StyledLine> {
            wrap(reason, width.saturating_sub(2))
                .into_iter()
                .map(|line| StyledLine::new().dim(format!("  {line}")))
                .collect()
        };
        match known {
            Known::Refused(reason) => {
                std::iter::once(StyledLine::new().warn("● ").hi("Not removable"))
                    .chain(reason_lines(reason))
                    .collect()
            }
            Known::Measuring => vec![spinner(ctx, "measuring what it left…")],
            Known::Failed(reason) => {
                std::iter::once(StyledLine::new().bad("● ").hi("Could not be measured"))
                    .chain(reason_lines(reason))
                    .collect()
            }
            Known::Ready(plan) => self.entry_lines(plan, width),
        }
    }

    fn entry_lines(&self, plan: &Plan, width: usize) -> Vec<StyledLine> {
        let library = self.library.as_deref();
        let path_width = width.saturating_sub(ENTRY_LABEL + ENTRY_SIZE + 1);
        let entries = plan.items.iter().flat_map(|item| match kept_reason(item) {
            Some(reason) => {
                let text = format!("{}: {reason}", kept_place(&item.path, library));
                wrap(&text, width.saturating_sub(ENTRY_LABEL))
                    .into_iter()
                    .enumerate()
                    .map(|(index, line)| {
                        let lead = if index == 0 { "kept" } else { "" };
                        StyledLine::new()
                            .dim(format!("{lead:<ENTRY_LABEL$}"))
                            .plain(line)
                    })
                    .collect::<Vec<_>>()
            }
            None => vec![StyledLine::new()
                .dim(format!("{:<ENTRY_LABEL$}", label(item)))
                .plain(format!("{:>ENTRY_SIZE$}", format_size(item.size)))
                .plain(" ")
                .dim(cut(&shown_path(&item.path, library), path_width))],
        });
        let removable = StyledLine::new()
            .dim("Removable  ")
            .hi(format_size(figures(plan).removable));
        entries.chain([StyledLine::new(), removable]).collect()
    }

    /// The band's last two lines below 100 columns.
    fn known_brief(known: &Known<'_>, ctx: &ViewCtx) -> Vec<StyledLine> {
        match known {
            Known::Refused(reason) => vec![StyledLine::new()
                .warn("● ")
                .dim(format!("Not removable: {reason}"))],
            Known::Measuring => vec![spinner(ctx, "measuring what it left…")],
            Known::Failed(reason) => {
                vec![StyledLine::new()
                    .bad("● ")
                    .dim(format!("Could not be measured: {reason}"))]
            }
            Known::Ready(plan) => {
                let sums = figures(plan);
                let kept = if sums.kept > 0 {
                    format!("   {} kept", plural(sums.kept, "entry", "entries"))
                } else {
                    String::new()
                };
                vec![
                    StyledLine::new()
                        .dim("app ")
                        .plain(format_size(sums.app))
                        .dim("   cache ")
                        .plain(format_size(sums.cache))
                        .dim("   data ")
                        .plain(format_size(sums.data))
                        .dim(kept),
                    StyledLine::new()
                        .dim("Removable  ")
                        .hi(format_size(sums.removable)),
                ]
            }
        }
    }

    /// How many apps are picked and what is removable for them.
    fn foot(&self) -> StyledLine {
        let picked = self.picked();
        let count = plural(picked.len(), "app", "apps");
        if picked.is_empty() {
            return StyledLine::new().dim(format!("{count} picked"));
        }
        let plans: Vec<Option<&PlanRead>> = picked
            .iter()
            .map(|row| self.plans.get(&row.app.path))
            .collect();
        if plans.iter().any(Option::is_none) {
            return StyledLine::new().dim(format!("{count} picked · measuring…"));
        }
        let bytes: u64 = plans
            .iter()
            .filter_map(|read| match read {
                Some(PlanRead::Ready(plan)) => Some(figures(plan).removable),
                _ => None,
            })
            .fold(0u64, u64::saturating_add);
        let failed = plans
            .iter()
            .filter(|read| matches!(read, Some(PlanRead::Failed(_))))
            .count();
        let line = StyledLine::new()
            .hi(format_size(bytes))
            .dim(format!(" removable in {count} picked"));
        if failed > 0 {
            line.dim("    ")
                .warn("● ")
                .dim(format!("{failed} not measured"))
        } else {
            line
        }
    }
}
