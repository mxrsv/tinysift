//! One clean as it runs and after: per category what moved, what was kept
//! and what did not run, in scan sizes of the paths the engine reports (I11).
//! The session adds up every clean for the exit card (TU4).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tiny_core::clean::execute::{GroupOutcome, ObservedRun};
use tiny_core::clean::types::CleanAction;

use super::engine::{Plan, Route};
use super::listing::Listing;
use super::wording::kept_reason;
use crate::tui::work::TaskHandle;

/// What one finished category did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// Scan sizes of the paths removed or whose removal a tool accepted.
    pub bytes: u64,
    /// A tool accepted the removal and finishes it in the background.
    pub requested: bool,
    /// Paths the engine kept, with its reason (`kept: ` taken off).
    pub kept: Vec<(PathBuf, String)>,
    pub kept_bytes: u64,
    /// Nothing moved and something was kept.
    pub whole_kept: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowState {
    Waiting,
    Finished(Moved),
    NotRun,
}

#[derive(Clone, Debug)]
pub struct RunRow {
    pub id: String,
    pub label: String,
    /// Removed for good rather than moved to the Trash.
    pub for_good: bool,
    pub state: RowState,
}

/// Bytes of a run or a session, split as the done screen and the exit card show them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub cleaned: u64,
    pub in_trash: u64,
    pub for_good: u64,
    pub kept: u64,
}

pub struct Run {
    pub action: CleanAction,
    pub rows: Vec<RunRow>,
    sizes: HashMap<PathBuf, u64>,
    pub started: Instant,
    pub took: Option<Duration>,
    pub stopping: bool,
    pub error: Option<String>,
    pub task: Option<TaskHandle>,
}

impl Run {
    pub fn new(plan: &Plan, listing: &Listing, started: Instant) -> Self {
        let route = |id: &str| {
            listing
                .entries
                .iter()
                .find(|e| e.category.id == id)
                .map(|e| e.category.route)
        };
        let rows = plan
            .groups
            .iter()
            .map(|group| RunRow {
                id: group.id.clone(),
                label: group.label.clone(),
                for_good: plan.action == CleanAction::HardDelete
                    || route(&group.id) == Some(Route::ForGood),
                state: RowState::Waiting,
            })
            .collect();
        Self {
            action: plan.action,
            rows,
            sizes: plan
                .handed()
                .map(|item| (item.path.clone(), item.size))
                .collect(),
            started,
            took: None,
            stopping: false,
            error: None,
            task: None,
        }
    }

    pub fn ended(&self) -> bool {
        self.took.is_some()
    }

    /// The category in progress: the first that has not finished.
    pub fn running(&self) -> Option<usize> {
        if self.ended() {
            return None;
        }
        self.rows.iter().position(|r| r.state == RowState::Waiting)
    }

    pub fn record(&mut self, outcome: &GroupOutcome) {
        let size = |path: &PathBuf| self.sizes.get(path).copied().unwrap_or(0);
        let report = &outcome.report;
        let bytes = report
            .removed_paths
            .iter()
            .chain(&report.removal_requested)
            .fold(0u64, |sum, path| sum.saturating_add(size(path)));
        let mut kept: Vec<(PathBuf, String)> = report
            .failed
            .iter()
            .map(|(path, reason)| (path.clone(), kept_reason(reason).to_string()))
            .collect();
        if let (Some(app), true) = (&report.skipped_running_app, kept.is_empty()) {
            // The engine names the app but no path: the reason stands for the category.
            kept.push((PathBuf::new(), format!("{app} is open")));
        }
        let kept_bytes = kept
            .iter()
            .fold(0u64, |sum, (path, _)| sum.saturating_add(size(path)));
        let moved_anything =
            !report.removed_paths.is_empty() || !report.removal_requested.is_empty();
        let moved = Moved {
            bytes,
            requested: !report.removal_requested.is_empty(),
            whole_kept: !moved_anything && !kept.is_empty(),
            kept,
            kept_bytes,
        };
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|r| r.id == outcome.category_id && r.state == RowState::Waiting)
        {
            row.state = RowState::Finished(moved);
        }
    }

    /// The engine returned: categories it did not reach did not run.
    pub fn end(&mut self, result: Result<ObservedRun, String>, took: Duration) {
        if let Err(error) = result {
            self.error = Some(error);
        }
        for row in &mut self.rows {
            if row.state == RowState::Waiting {
                row.state = RowState::NotRun;
            }
        }
        self.took = Some(took);
        self.task = None;
    }

    pub fn totals(&self) -> Totals {
        self.rows.iter().fold(Totals::default(), |sum, row| {
            let RowState::Finished(moved) = &row.state else {
                return sum;
            };
            let (in_trash, for_good) = if row.for_good {
                (0, moved.bytes)
            } else {
                (moved.bytes, 0)
            };
            Totals {
                cleaned: sum.cleaned.saturating_add(moved.bytes),
                in_trash: sum.in_trash.saturating_add(in_trash),
                for_good: sum.for_good.saturating_add(for_good),
                kept: sum.kept.saturating_add(moved.kept_bytes),
            }
        })
    }

    /// The first category that moved something but kept a path: its path and reason.
    pub fn partly_kept(&self) -> Option<(&RunRow, &Moved)> {
        self.rows.iter().find_map(|row| match &row.state {
            RowState::Finished(moved) if !moved.whole_kept && !moved.kept.is_empty() => {
                Some((row, moved))
            }
            _ => None,
        })
    }
}

/// Every clean of the session, for the exit card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    /// `(id, label, bytes)` of each category that cleaned something, first run first.
    pub rows: Vec<(String, String, u64)>,
    pub totals: Totals,
    pub took: Duration,
    pub cleans: usize,
}

impl Session {
    /// Adds a run that reached at least one category; one the engine refused
    /// before any category cleaned nothing and is left out.
    pub fn add(&mut self, run: &Run) {
        if !run
            .rows
            .iter()
            .any(|row| matches!(row.state, RowState::Finished(_)))
        {
            return;
        }
        self.cleans += 1;
        self.took += run.took.unwrap_or_default();
        let totals = run.totals();
        self.totals = Totals {
            cleaned: self.totals.cleaned.saturating_add(totals.cleaned),
            in_trash: self.totals.in_trash.saturating_add(totals.in_trash),
            for_good: self.totals.for_good.saturating_add(totals.for_good),
            kept: self.totals.kept.saturating_add(totals.kept),
        };
        for row in &run.rows {
            let RowState::Finished(moved) = &row.state else {
                continue;
            };
            if moved.whole_kept {
                continue;
            }
            match self.rows.iter_mut().find(|(id, _, _)| *id == row.id) {
                Some((_, _, bytes)) => *bytes = bytes.saturating_add(moved.bytes),
                None => self
                    .rows
                    .push((row.id.clone(), row.label.clone(), moved.bytes)),
            }
        }
    }
}
