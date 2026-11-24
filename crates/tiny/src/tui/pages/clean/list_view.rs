//! The category list: its rows, the highlighted row's detail, the foot and
//! the keys, while measuring, listed, failed, stopped or empty.

use crate::tui::icons::IconMode;
use crate::tui::pages::{Detail, KeyHint, PageView, ViewCtx};
use crate::tui::styled_line::{Role, StyledLine};
use crate::tui::widgets::meter::{meter, name_and_bar, BAR};
use crate::tui::widgets::phrases::plural;
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::spinner::spinner_line;
use crate::tui::widgets::text_fit::{cut, fill, middle_ellipsis, wrap};

use super::engine::{Finding, Route};
use super::listing::{Entry, Listing};
use super::wording::{comes_back, shown_path};

/// Cells a row spends besides the name: lead, tick, bar, size and tag, with
/// the bar at its full width.
const ROW_CHROME: usize = 39;
const LABEL: usize = 12;

/// Where a read stands while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measuring {
    pub done: u64,
    pub total: u64,
}

pub struct ListScreen<'a> {
    pub listing: Option<&'a Listing>,
    pub cursor: usize,
    pub review: bool,
}

impl ListScreen<'_> {
    /// The list while a read runs: the rows of the last read (if any), then the progress line.
    pub fn measuring(&self, progress: Measuring, ctx: &ViewCtx) -> PageView {
        let spinner = spinner_line(
            ctx.tick,
            if progress.total == 0 {
                "measuring…".to_string()
            } else {
                format!(
                    "measuring… {} of {} categories",
                    progress.done, progress.total
                )
            },
        );
        let rows = self.rows(ctx);
        let cursor = (!rows.is_empty()).then_some(self.cursor);
        PageView {
            list: rows.into_iter().chain(std::iter::once(spinner)).collect(),
            cursor,
            detail: cursor.and_then(|_| self.detail(ctx)),
            foot: Some(StyledLine::new().dim("Ticking opens when every category is measured.")),
            keys: vec![
                KeyHint::new("esc", "stop"),
                KeyHint::new("tab", "page"),
                KeyHint::new("q", "quit"),
            ],
            ..PageView::default()
        }
    }

    /// `esc` stopped the read: nothing is listed and nothing can be cleaned.
    pub fn stopped(&self) -> PageView {
        PageView {
            list: vec![
                StyledLine::new(),
                StyledLine::new().warn("● ").hi("Measuring stopped"),
                StyledLine::new().dim("  r measures again"),
            ],
            keys: self.reread_keys(true),
            ..PageView::default()
        }
    }

    pub fn listed(&self, ctx: &ViewCtx) -> PageView {
        let Some(listing) = self.listing else {
            return self.stopped();
        };
        if listing.entries.is_empty() {
            return self.empty(listing);
        }
        let rows = self.rows(ctx);
        let hint = if listing.cleaning_off() {
            Vec::new()
        } else {
            self.view_hint()
        };
        let base = PageView {
            list: rows.into_iter().chain(hint).collect(),
            cursor: Some(self.cursor),
            detail: self.detail(ctx),
            ..PageView::default()
        };
        if listing.cleaning_off() {
            return PageView {
                foot: Some(
                    StyledLine::new()
                        .bad("● ")
                        .dim("Nothing is cleaned until every listed category is read."),
                ),
                keys: self.reread_keys(listing.view_switch_allowed(self.review)),
                ..base
            };
        }
        PageView {
            foot: Some(foot(listing)),
            keys: [
                KeyHint::new("↑↓", "move"),
                KeyHint::new("space", "tick"),
                KeyHint::new("v", self.swap()),
                KeyHint::new("p", "paths"),
                KeyHint::new("enter", "clean"),
            ]
            .into_iter()
            .chain(KeyHint::globals())
            .collect(),
            ..base
        }
    }

    fn empty(&self, listing: &Listing) -> PageView {
        if let Some(failure) = &listing.failure {
            return PageView {
                list: vec![
                    StyledLine::new(),
                    StyledLine::new().bad("● ").hi("Nothing could be read"),
                    StyledLine::new().dim(format!("  {failure}")),
                ],
                keys: self.reread_keys(false),
                ..PageView::default()
            };
        }
        let message = if self.review {
            StyledLine::new().ok("✓ ").hi("Nothing to clean")
        } else {
            StyledLine::new().ok("✓ ").hi("Nothing safe to clean")
        };
        PageView {
            list: [StyledLine::new(), message]
                .into_iter()
                .chain(self.view_hint())
                .collect(),
            keys: [
                KeyHint::new("v", self.swap()),
                KeyHint::new("r", "read again"),
            ]
            .into_iter()
            .chain(KeyHint::globals())
            .collect(),
            ..PageView::default()
        }
    }

    fn swap(&self) -> &'static str {
        if self.review {
            "safe only"
        } else {
            "review"
        }
    }

    fn reread_keys(&self, offer_view: bool) -> Vec<KeyHint> {
        let view = if self.review {
            KeyHint::new("v", "back to safe only")
        } else {
            KeyHint::new("v", "review")
        };
        offer_view
            .then_some(view)
            .into_iter()
            .chain(std::iter::once(KeyHint::new("r", "read again")))
            .chain(KeyHint::globals())
            .collect()
    }

    fn view_hint(&self) -> Vec<StyledLine> {
        if self.review {
            Vec::new()
        } else {
            vec![
                StyledLine::new(),
                StyledLine::new().dim("    v also lists what is worth a look first"),
            ]
        }
    }

    fn rows(&self, ctx: &ViewCtx) -> Vec<StyledLine> {
        let Some(listing) = self.listing else {
            return Vec::new();
        };
        let max = listing.meter_max();
        listing
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| row(entry, index == self.cursor, max, ctx))
            .collect()
    }

    fn detail(&self, ctx: &ViewCtx) -> Option<Detail> {
        let listing = self.listing?;
        let entry = listing.entries.get(self.cursor)?;
        Some(category_detail(entry, listing, ctx.detail_width))
    }
}

fn row(entry: &Entry, highlighted: bool, max: u64, ctx: &ViewCtx) -> StyledLine {
    let category = &entry.category;
    let icon = ctx.icons.category_prefix(&category.id);
    let lead = if highlighted {
        StyledLine::new().hi("❯ ")
    } else {
        StyledLine::from("  ")
    };
    if let Finding::Failed { reason } = &category.finding {
        return StyledLine::new()
            .bad("● ")
            .plain("  ")
            .dim(icon)
            .plain(category.label.as_str())
            .dim(format!("   not read: {reason}"));
    }
    let (name_width, bar) = name_and_bar(
        ctx.list_width
            .saturating_sub(ROW_CHROME - BAR + if ctx.icons == IconMode::Nerd { 2 } else { 0 }),
    );
    let on = entry.ticked && entry.tickable();
    let tick = if entry.locked() {
        StyledLine::new().faint("– ")
    } else if on {
        StyledLine::new().hi("◉ ")
    } else {
        StyledLine::new().faint("◯ ")
    };
    let name = fill(&category.label, name_width);
    let running = matches!(category.finding, Finding::AppRunning { .. });
    let size = if running {
        String::new()
    } else {
        format_size(entry.bytes())
    };
    let fraction = if entry.locked() {
        0.0
    } else {
        entry.bytes() as f64 / max as f64
    };
    let line = lead
        .append(tick)
        .dim(icon)
        .push(
            if on {
                Role::Hi
            } else if entry.locked() {
                Role::Dim
            } else {
                Role::Plain
            },
            name,
        )
        .plain(" ")
        .append(meter(fraction, bar, Role::Storage))
        .plain(" ")
        .push(if on { Role::Hi } else { Role::Dim }, format!("{size:>10}"))
        .plain("  ");
    if category.route == Route::ReportOnly {
        line.dim("report")
    } else if running {
        line.warn("in use")
    } else if category.risk == tiny_core::clean::types::RiskLevel::Review {
        line.warn("review")
    } else {
        line
    }
}

/// The foot: what enter cleans, split into what goes to the Trash and what a tool removes for good.
pub fn foot(listing: &Listing) -> StyledLine {
    let total = listing
        .plan(tiny_core::clean::types::CleanAction::Trash)
        .bytes();
    let gone = listing.for_good_bytes();
    let count = listing.picked().count();
    let line = StyledLine::new()
        .hi(format_size(total))
        .dim(format!(
            " picked in {}    ",
            plural(count, "category", "categories")
        ))
        .ok("● ")
        .dim(format!("Trash {}", format_size(total - gone)));
    if gone > 0 {
        line.dim("    ")
            .bad("● ")
            .dim(format!("for good {}", format_size(gone)))
    } else {
        line
    }
}

fn category_detail(entry: &Entry, listing: &Listing, width: usize) -> Detail {
    let category = &entry.category;
    let (what, label, why, why_role) = match &category.finding {
        Finding::Found(group) => (
            format!(
                "{} · {}",
                format_size(group.total_size),
                plural(group.items.len(), "item", "items")
            ),
            "Listed      ",
            category.reason.clone(),
            Role::Plain,
        ),
        Finding::AppRunning { app } => (
            "not measured".to_string(),
            "Skipped     ",
            format!("not measured: {app} is open. Quit it, then press r"),
            Role::Warn,
        ),
        Finding::Failed { reason } => (
            "not read".to_string(),
            "Not read    ",
            format!("{reason}. r reads it again"),
            Role::Warn,
        ),
    };
    let goes = if entry.locked() || entry.group().is_none() {
        StyledLine::new().dim("Goes to     nowhere")
    } else if category.route == Route::ForGood {
        StyledLine::new()
            .dim("Goes to     ")
            .bad("removed for good")
    } else {
        StyledLine::new()
            .dim("Goes to     ")
            .hi("Trash")
            .dim("  Finder › Put Back")
    };
    let back = comes_back(category.comes_back);
    let why_lines: Vec<StyledLine> = wrap(&why, width.saturating_sub(LABEL))
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let lead = if i == 0 { label } else { "            " };
            StyledLine::new().dim(lead).push(why_role, line)
        })
        .collect();
    let largest = entry.group().map_or_else(Vec::new, |group| {
        let mut items: Vec<_> = group.items.iter().collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.size));
        items.truncate(2);
        items
    });
    let more = entry
        .group()
        .map_or(0, |group| group.items.len().saturating_sub(largest.len()));
    let tail = largest
        .iter()
        .map(|item| {
            StyledLine::new().dim(middle_ellipsis(
                &shown_path(&item.path, listing.home.as_ref()),
                width,
            ))
        })
        .chain((more > 0 && !largest.is_empty()).then(|| {
            StyledLine::new().faint(format!(
                "… {} more · p lists them",
                crate::tui::widgets::size_format::format_count(more as u64)
            ))
        }));
    let full = wrap(&category.label, width)
        .into_iter()
        .map(|line| StyledLine::new().hi(line))
        .chain([
            StyledLine::new().dim(what.as_str()),
            StyledLine::new(),
            goes.clone(),
            StyledLine::new().dim("Comes back  ").plain(back),
        ])
        .chain(why_lines)
        .chain(std::iter::once(StyledLine::new()))
        .chain(tail)
        .collect();
    let brief = vec![
        StyledLine::new()
            .hi(cut(&category.label, width.saturating_sub(30)))
            .dim(format!("  {what}")),
        goes.dim("    comes back ").plain(back),
        StyledLine::new().dim(label).push(why_role, why),
    ];
    Detail {
        title: "Highlighted".into(),
        full,
        brief,
    }
}
