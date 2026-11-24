//! One clean as it runs and after: per category what moved, what a tool was
//! asked to remove, what was kept, where the engine stopped and what did not
//! run, in scan sizes of the paths the engine reports (I11). The session adds
//! up every clean for the exit card (TU4).

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
    /// Scan sizes of the paths the engine removed.
    pub bytes: u64,
    /// Scan sizes of the paths a tool was asked to remove and finishes in the
    /// background: not cleaned yet, so counted apart (S8).
    pub requested: u64,
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
    /// The engine stopped inside this category with this reason; what it
    /// did before is not reported, so it may be partly done (S9).
    Stopped(String),
    /// The run ended before this category.
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
    /// In Trash plus For good.
    pub cleaned: u64,
    pub in_trash: u64,
    pub for_good: u64,
    /// Asked of a tool, not yet removed.
    pub requested: u64,
    pub kept: u64,
}

impl Totals {
    fn add(self, other: Totals) -> Totals {
        Totals {
            cleaned: self.cleaned.saturating_add(other.cleaned),
            in_trash: self.in_trash.saturating_add(other.in_trash),
            for_good: self.for_good.saturating_add(other.for_good),
            requested: self.requested.saturating_add(other.requested),
            kept: self.kept.saturating_add(other.kept),
        }
    }
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
        let sum = |paths: &[PathBuf]| {
            paths
                .iter()
                .fold(0u64, |sum, path| sum.saturating_add(size(path)))
        };
        let report = &outcome.report;
        let row = self
            .rows
            .iter_mut()
            .find(|r| r.id == outcome.category_id && r.state == RowState::Waiting);
        if let (Some(row), Some(reason)) = (row, &outcome.stopped) {
            row.state = RowState::Stopped(reason.clone());
            return;
        }
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
            bytes: sum(&report.removed_paths),
            requested: sum(&report.removal_requested),
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

    /// The engine returned: categories it did not reach did not run. An
    /// error the engine stopped a category with is already on that row.
    pub fn end(&mut self, result: Result<ObservedRun, String>, took: Duration) {
        if let Err(error) = result {
            if self.stopped_in().is_none() {
                self.error = Some(error);
            }
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
            sum.add(Totals {
                cleaned: moved.bytes,
                in_trash,
                for_good,
                requested: moved.requested,
                kept: moved.kept_bytes,
            })
        })
    }

    /// The category the engine stopped inside, and its reason.
    pub fn stopped_in(&self) -> Option<(&RunRow, &str)> {
        self.rows.iter().find_map(|row| match &row.state {
            RowState::Stopped(reason) => Some((row, reason.as_str())),
            _ => None,
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
        self.totals = self.totals.add(run.totals());
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
