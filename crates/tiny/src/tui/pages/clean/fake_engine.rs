//! A cleanup engine for tests: it lists the mock's data (`DATA` in
//! `tools/mock.js`) and records every plan it is handed. It never touches a
//! file; "removing" a path only reports it removed.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex};

use tiny_core::clean::execute::{GroupOutcome, ObservedRun};
use tiny_core::clean::types::{ExecReport, RiskLevel};

use super::engine::{Category, CleanEngine, Discovery, Finding, GroupObserver, Plan, Progressed};
use super::fake_data::{HOME, READ_AT};

#[derive(Default)]
pub struct FakeEngine {
    /// Every category the fake knows; a read returns the Safe ones, and the
    /// Review ones too when asked (as `sift clean --include-review`).
    pub categories: Mutex<Vec<Category>>,
    pub failure: Mutex<Option<String>>,
    /// Paths the fake keeps, with the engine's reason.
    pub keep: Mutex<HashMap<PathBuf, String>>,
    /// The error `execute` returns before any group, as a busy kept-paths store does.
    pub refuse: Mutex<Option<String>>,
    pub plans: Mutex<Vec<Plan>>,
    pub reads: Mutex<Vec<bool>>,
    /// While true, `execute` waits before its first group.
    held: Mutex<bool>,
    released: Condvar,
}

impl FakeEngine {
    pub fn with(categories: Vec<Category>) -> Self {
        Self {
            categories: Mutex::new(categories),
            ..Self::default()
        }
    }

    pub fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    pub fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }

    pub fn keep(&self, path: &str, reason: &str) {
        self.keep
            .lock()
            .unwrap()
            .insert(PathBuf::from(path), reason.into());
    }
}

impl CleanEngine for FakeEngine {
    fn discover(&self, review: bool, progress: Progressed<'_>, _cancel: &AtomicBool) -> Discovery {
        self.reads.lock().unwrap().push(review);
        let categories: Vec<Category> = self
            .categories
            .lock()
            .unwrap()
            .iter()
            .filter(|c| review || c.risk == RiskLevel::Safe)
            .cloned()
            .collect();
        let total = categories.len() as u64;
        for done in 0..=total {
            progress(done, total);
        }
        // As the engine does: a failed category fails the read.
        let failed = categories.iter().find_map(|c| match c.finding {
            Finding::Failed { .. } => Some(format!("{}: scan failed", c.id)),
            _ => None,
        });
        Discovery {
            failure: self.failure.lock().unwrap().clone().or(failed),
            categories,
            read_at: READ_AT,
            home: Some(PathBuf::from(HOME)),
        }
    }

    fn execute(&self, plan: &Plan, on_group: GroupObserver<'_>) -> Result<ObservedRun, String> {
        self.plans.lock().unwrap().push(plan.clone());
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.released.wait(held).unwrap();
        }
        drop(held);
        if let Some(error) = self.refuse.lock().unwrap().clone() {
            return Err(error);
        }
        let keep = self.keep.lock().unwrap().clone();
        let mut run = ObservedRun::default();
        for (index, group) in plan.groups.iter().enumerate() {
            let mut report = ExecReport::default();
            for item in group
                .items
                .iter()
                .filter(|i| !plan.excluded.contains(&i.path))
            {
                match keep.get(&item.path) {
                    Some(reason) => report.failed.push((item.path.clone(), reason.clone())),
                    None => report.removed_paths.push(item.path.clone()),
                }
            }
            let outcome = GroupOutcome {
                category_id: group.id.clone(),
                report,
            };
            let flow = on_group(&outcome);
            run.report.merge(outcome.report);
            if flow == ControlFlow::Break(()) {
                run.not_run = plan.groups[index + 1..]
                    .iter()
                    .map(|g| g.id.clone())
                    .collect();
                break;
            }
        }
        Ok(run)
    }
}
