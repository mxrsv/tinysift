//! One read's categories as the page lists them: sorted, ticked and with
//! the paths unticked in the path list. Ticks start where `sift clean`'s
//! picker starts them: Safe ticked, Review not. A read again keeps the
//! user's choices (S2): a category keeps its tick by id, an unticked path
//! stays unticked; only a category seen for the first time takes the
//! default, and what disappeared is dropped.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use tiny_core::clean::discover::CategoryGroup;
use tiny_core::clean::types::{CleanAction, RiskLevel};

use super::engine::{Category, Discovery, Finding, Plan, Route};

#[derive(Clone, Debug)]
pub struct Entry {
    pub category: Category,
    pub ticked: bool,
}

impl Entry {
    pub fn group(&self) -> Option<&CategoryGroup> {
        match &self.category.finding {
            Finding::Found(group) => Some(group),
            _ => None,
        }
    }

    /// Found and not report-only: the engine would act on it.
    pub fn tickable(&self) -> bool {
        self.group().is_some() && self.category.route != Route::ReportOnly
    }

    /// Report-only or not measured: drawn with `–` and never ticked.
    pub fn locked(&self) -> bool {
        matches!(self.category.finding, Finding::AppRunning { .. })
            || self.category.route == Route::ReportOnly
    }

    pub fn bytes(&self) -> u64 {
        self.group().map_or(0, |group| group.total_size)
    }
}

/// The user's choices in one listing, carried into the next read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    ticked: HashMap<String, bool>,
    excluded: HashSet<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub excluded: HashSet<PathBuf>,
    pub failure: Option<String>,
    pub read_at: i64,
    pub home: Option<PathBuf>,
}

impl Listing {
    /// Largest first (I22); categories without a size after those with one,
    /// failed reads last. Destructive categories never reach the TUI (TU10).
    /// `carried` are the choices of the listing this read replaces.
    pub fn new(discovery: Discovery, carried: Option<&Choices>) -> Self {
        let mut entries: Vec<Entry> = discovery
            .categories
            .into_iter()
            .filter(|category| category.risk != RiskLevel::Destructive)
            .map(|category| Entry {
                ticked: false,
                category,
            })
            .collect();
        for entry in &mut entries {
            let default = entry.category.risk == RiskLevel::Safe;
            let kept = carried.and_then(|c| c.ticked.get(&entry.category.id).copied());
            entry.ticked = entry.tickable() && kept.unwrap_or(default);
        }
        // An unticked path stays unticked while its category still lists it.
        let excluded = carried.map_or_else(HashSet::new, |carried| {
            entries
                .iter()
                .filter_map(Entry::group)
                .flat_map(|group| &group.items)
                .filter(|item| carried.excluded.contains(&item.path))
                .map(|item| item.path.clone())
                .collect()
        });
        entries.sort_by_key(|entry| {
            let rank = match entry.category.finding {
                Finding::Found(_) => 0,
                Finding::AppRunning { .. } => 1,
                Finding::Failed { .. } => 2,
            };
            (rank, std::cmp::Reverse(entry.bytes()))
        });
        Self {
            entries,
            excluded,
            failure: discovery.failure,
            read_at: discovery.read_at,
            home: discovery.home,
        }
    }

    /// What the user chose here, for the next read. A category listed but
    /// not measured this time (its app runs, its read failed) keeps the tick
    /// it carried in; one no longer listed is dropped.
    pub fn choices(&self, before: Option<&Choices>) -> Choices {
        let carried = self
            .entries
            .iter()
            .filter(|e| !e.tickable())
            .filter_map(|e| {
                let id = &e.category.id;
                before
                    .and_then(|b| b.ticked.get(id))
                    .map(|&ticked| (id.clone(), ticked))
            });
        let chosen = self
            .entries
            .iter()
            .filter(|e| e.tickable())
            .map(|e| (e.category.id.clone(), e.ticked));
        Choices {
            ticked: carried.chain(chosen).collect(),
            excluded: self.excluded.clone(),
        }
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(entry) = self.entries.get_mut(index).filter(|e| e.tickable()) {
            entry.ticked = !entry.ticked;
        }
    }

    pub fn picked(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.ticked && e.tickable())
    }

    /// Nothing may be cleaned: a category failed, or the read as a whole.
    pub fn cleaning_off(&self) -> bool {
        self.failure.is_some()
            || self
                .entries
                .iter()
                .any(|e| matches!(e.category.finding, Finding::Failed { .. }))
    }

    /// A Safe category failed (or the read failed as a whole): only `r` goes on (I13).
    pub fn safe_failed(&self) -> bool {
        let failed: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| matches!(e.category.finding, Finding::Failed { .. }))
            .collect();
        (failed.is_empty() && self.failure.is_some())
            || failed.iter().any(|e| e.category.risk == RiskLevel::Safe)
    }

    /// `v` may switch views: nothing failed, or only Review categories did and
    /// `v` returns to Safe only (I13).
    pub fn view_switch_allowed(&self, review: bool) -> bool {
        !self.cleaning_off() || (review && !self.safe_failed())
    }

    /// What enter hands to the engine for `action`.
    pub fn plan(&self, action: CleanAction) -> Plan {
        Plan {
            action,
            groups: self.picked().filter_map(|e| e.group().cloned()).collect(),
            excluded: self.excluded.clone(),
        }
    }

    /// Of the picked bytes, those a tool removes for good under Move to Trash.
    pub fn for_good_bytes(&self) -> u64 {
        let tools = Listing {
            entries: self
                .picked()
                .filter(|e| e.category.route == Route::ForGood)
                .cloned()
                .collect(),
            excluded: self.excluded.clone(),
            ..Listing::default()
        };
        tools.plan(CleanAction::Trash).bytes()
    }

    /// The largest size a meter is drawn against: locked rows do not count.
    pub fn meter_max(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| !e.locked())
            .map(Entry::bytes)
            .max()
            .unwrap_or(0)
            .max(1)
    }
}
