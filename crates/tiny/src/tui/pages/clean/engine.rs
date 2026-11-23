//! The only way the Clean page reaches the cleanup engine. What may be
//! listed, ticked, refused or removed is decided by `tiny-core`; this module
//! carries its answers to the page and adds no rule of its own.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use tiny_core::clean::discover::{discover_for_cli_with, select_providers, CategoryGroup};
use tiny_core::clean::execute::{execute_observed, GroupOutcome, ObservedRun, ReviewExclusion};
use tiny_core::clean::types::{CleanAction, CleanItem, ComesBack, RiskLevel};
use tiny_core::options::CleanOptions;
use tiny_core::progress::Progress;

use super::discovery_map;

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
    /// The items the engine acts on: every item of every group but those the
    /// engine keeps for the unticked paths, which are the unticked paths and
    /// any item above or below one ([`ReviewExclusion`], S7).
    pub fn handed(&self) -> impl Iterator<Item = &CleanItem> {
        let rule = ReviewExclusion::new(&self.excluded);
        self.groups
            .iter()
            .flat_map(|group| &group.items)
            .filter(move |item| !rule.keeps(&item.path))
    }

    /// [`handed`](Self::handed), group by group.
    pub fn handed_in<'a>(
        &'a self,
        group: &'a CategoryGroup,
    ) -> impl Iterator<Item = &'a CleanItem> {
        let rule = ReviewExclusion::new(&self.excluded);
        group
            .items
            .iter()
            .filter(move |item| !rule.keeps(&item.path))
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
        match discover_for_cli_with(&opts, Some(&report), Some(cancel)) {
            Ok(found) => Discovery {
                categories: discovery_map::categories(&select_providers(&opts), &found),
                failure: discovery_map::failure(&found),
                read_at,
                home,
            },
            Err(error) => Discovery {
                failure: Some(error.to_string()),
                read_at,
                home,
                ..Discovery::default()
            },
        }
    }

    fn execute(&self, plan: &Plan, on_group: GroupObserver<'_>) -> Result<ObservedRun, String> {
        let groups: Vec<&CategoryGroup> = plan.groups.iter().collect();
        let opts = clean_options(false);
        execute_observed(&groups, plan.action, &plan.excluded, &opts, on_group)
            .map_err(|error| error.to_string())
    }
}
