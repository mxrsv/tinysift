//! The only way the Clean page reaches the cleanup engine. What may be
//! listed, ticked, refused or removed is decided by `tiny-core`; this module
//! carries its answers to the page and adds no rule of its own.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use tiny_core::clean::discover::{discover_for_cli_with, select_providers, CategoryGroup};
use tiny_core::clean::execute::{execute_observed, GroupOutcome, ObservedRun};
use tiny_core::clean::providers::{comes_back, desktop_report_only, ReportOnly};
use tiny_core::clean::types::{CleanAction, ComesBack, RiskLevel};
use tiny_core::options::CleanOptions;
use tiny_core::progress::Progress;

/// What a category's items go through when they are cleaned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Each path moves to the Trash (Move to Trash) or is deleted (Hard delete).
    Trash,
    /// A tool command removes them for good whichever action is chosen.
    ForGood,
    /// The engine only reports them; nothing is ever removed.
    ReportOnly,
}

/// What the engine's discovery said about one category.
#[derive(Clone, Debug)]
pub enum Finding {
    Found(CategoryGroup),
    /// Not scanned: its app runs.
    AppRunning {
        app: String,
    },
    /// The read failed, so nothing may be cleaned until it is read again.
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct Category {
    pub id: String,
    pub label: String,
    pub risk: RiskLevel,
    /// Why its items are candidates (`CleanProvider::inclusion_reason`).
    pub reason: String,
    pub comes_back: Option<ComesBack>,
    pub route: Route,
    pub finding: Finding,
}

/// One read of the categories `sift clean` lists.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    pub categories: Vec<Category>,
    /// The engine refused cleanup after this read; the text says why.
    pub failure: Option<String>,
    /// Unix seconds when the read ran, for the age of each path's evidence.
    pub read_at: i64,
    /// Shown as `~` in paths.
    pub home: Option<PathBuf>,
}

/// What enter hands to the engine: the ticked categories, whole, and the
/// paths unticked in the path list, as `sift clean`'s drill-down does.
#[derive(Clone, Debug)]
pub struct Plan {
    pub action: CleanAction,
    pub groups: Vec<CategoryGroup>,
    pub excluded: HashSet<PathBuf>,
}

impl Plan {
    /// The items the engine is asked to act on: every item of every group but the unticked paths.
    pub fn handed(&self) -> impl Iterator<Item = &tiny_core::clean::types::CleanItem> {
        self.groups
            .iter()
            .flat_map(|group| &group.items)
            .filter(|item| !self.excluded.contains(&item.path))
    }

    /// Sum of the scan sizes of the handed items: the figure on the foot and the action box.
    pub fn bytes(&self) -> u64 {
        self.handed()
            .fold(0u64, |sum, item| sum.saturating_add(item.size))
    }
}

/// `(categories done, categories in all)` as the read goes.
pub type Progressed<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

pub type GroupObserver<'a> = &'a mut dyn FnMut(&GroupOutcome) -> ControlFlow<()>;

pub trait CleanEngine: Send + Sync {
    /// The categories `sift clean` lists, with `--include-review` when
    /// `review`. `progress(done, total)` follows each category; `cancel`
    /// stops the read, which then counts as failed.
    fn discover(&self, review: bool, progress: Progressed<'_>, cancel: &AtomicBool) -> Discovery;

    /// Runs `plan` under one kept-paths lease, calling `on_group` after each category.
    fn execute(&self, plan: &Plan, on_group: GroupObserver<'_>) -> Result<ObservedRun, String>;
}

/// The real engine: `tiny-core`'s CLI discovery and executor.
pub struct CoreEngine;

/// `sift clean`'s own flags, so the options are exactly what the command uses.
#[derive(Parser)]
struct CleanCommand {
    #[command(flatten)]
    opts: crate::cli::CleanOpts,
}

/// The options of `sift clean`, plus `--include-review` for the Review view.
/// Never `--include-destructive` or `--category` (TU10).
pub fn clean_options(review: bool) -> CleanOptions {
    let args: &[&str] = if review {
        &["sift clean", "--include-review"]
    } else {
        &["sift clean"]
    };
    let command = CleanCommand::parse_from(args);
    (&command.opts).into()
}

impl CleanEngine for CoreEngine {
    fn discover(&self, review: bool, progress: Progressed<'_>, cancel: &AtomicBool) -> Discovery {
        let opts = clean_options(review);
        let read_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let report = |p: Progress| progress(p.completed, p.total.unwrap_or(0));
        let found = match discover_for_cli_with(&opts, Some(&report), Some(cancel)) {
            Ok(found) => found,
            Err(error) => {
                return Discovery {
                    failure: Some(error.to_string()),
                    read_at,
                    home,
                    ..Discovery::default()
                }
            }
        };
        let categories = select_providers(&opts)
            .iter()
            .filter_map(|provider| {
                let id = provider.id();
                let route = match desktop_report_only(provider.as_ref()) {
                    None => Route::Trash,
                    Some(ReportOnly::NotPerPathTrash) => Route::ForGood,
                    Some(ReportOnly::Destructive) => return None,
                    Some(_) => Route::ReportOnly,
                };
                let finding = if let Some((_, reason)) =
                    found.failed_categories.iter().find(|(c, _)| c == id)
                {
                    Finding::Failed {
                        reason: reason.clone(),
                    }
                } else if let Some(group) = found.report.groups.iter().find(|g| g.id == id) {
                    Finding::Found(group.clone())
                } else if let Some((_, app)) =
                    found.report.skipped_running.iter().find(|(c, _)| c == id)
                {
                    Finding::AppRunning { app: app.clone() }
                } else {
                    return None;
                };
                Some(Category {
                    id: id.to_string(),
                    label: provider.label().to_string(),
                    risk: provider.risk(),
                    reason: provider.inclusion_reason(),
                    comes_back: comes_back(id),
                    route,
                    finding,
                })
            })
            .collect();
        Discovery {
            categories,
            failure: found.failed.then(|| {
                found
                    .diagnostics
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "a category could not be read".into())
            }),
            read_at,
            home,
        }
    }

    fn execute(&self, plan: &Plan, on_group: GroupObserver<'_>) -> Result<ObservedRun, String> {
        let groups: Vec<&CategoryGroup> = plan.groups.iter().collect();
        let opts = clean_options(false);
        execute_observed(&groups, plan.action, &plan.excluded, &opts, on_group)
            .map_err(|error| error.to_string())
    }
}
