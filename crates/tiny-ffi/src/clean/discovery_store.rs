//! What the session keeps of its scans. A scan replaces only the categories
//! it covers (`docs/internals/clean.md`), so the store merges: candidates of
//! other categories keep their IDs, options and fingerprints, and everything
//! derived across candidates is recomputed over the merged set.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tiny_core::clean::freed_size::SharedBlocks;
use tiny_core::clean::fs_safe::PathFingerprint;
use tiny_core::clean::protection::RuleScope;
use tiny_core::clean::providers::DesktopTool;
use tiny_core::clean::types::CleanItem;
use tiny_core::options::CleanOptions;

use super::{FfiCleanCategory, FfiDesktopAction, FfiDiscovery};

#[derive(Default)]
pub(super) struct StoredDiscovery {
    /// The latest completed scan. It changes with every scan, whichever
    /// categories it covered.
    pub(super) id: u64,
    /// In display order: a rescanned category keeps its place, a new one
    /// goes last.
    categories: Vec<FfiCleanCategory>,
    pub(super) candidates: HashMap<String, StoredCandidate>,
}

pub(super) struct StoredCandidate {
    /// `item.category_id` is the category it was found in
    /// (`FfiCleanCategory::id`).
    pub(super) item: CleanItem,
    /// The options its category was scanned with; removal rebuilds the
    /// category's provider from them.
    pub(super) options: Arc<CleanOptions>,
    /// `None` when the path could not be stat'ed (e.g. a tool placeholder).
    pub(super) fingerprint: Option<PathFingerprint>,
    pub(super) roots: Arc<Vec<PathBuf>>,
    pub(super) desktop_action: FfiDesktopAction,
    /// What "Always keep" would cover, judged once from the path's own metadata.
    pub(super) keep_scope: Option<RuleScope>,
    pub(super) size_is_upper_bound: bool,
    /// Every shared block the walk saw. Which of them another category can
    /// complete is only known once all categories are in, so this is kept
    /// whole and `shared` is derived from it.
    pub(super) shared_seen: SharedBlocks,
    /// Only what other movable candidates could complete
    /// (`SharedBlocks::retain_completable`).
    pub(super) shared: SharedBlocks,
    /// The category's own tool, when the app asked for tool commands.
    pub(super) tool: Option<DesktopTool>,
}

/// The candidates of one completed scan, with fresh IDs.
pub(super) struct Scan {
    pub(super) id: u64,
    /// The categories the scan replaces; `None` is every category.
    pub(super) covers: Option<Vec<String>>,
    pub(super) categories: Vec<FfiCleanCategory>,
    pub(super) candidates: HashMap<String, StoredCandidate>,
}

impl StoredDiscovery {
    /// Replaces the scan's categories and keeps the rest. A category the scan
    /// covered but did not report is gone from this Mac, so it is dropped too.
    pub(super) fn merge(&mut self, scan: Scan) {
        let Some(covers) = scan.covers else {
            // A full scan is the whole picture, in scan order.
            let shares =
                shares_blocks(self.candidates.values()) || shares_blocks(scan.candidates.values());
            self.categories = scan.categories;
            self.candidates = scan.candidates;
            self.id = scan.id;
            if shares {
                self.keep_completable_shared_blocks();
            }
            return;
        };
        let replaced: HashSet<String> = covers
            .into_iter()
            .chain(scan.categories.iter().map(|c| c.id.clone()))
            .collect();
        let in_replaced =
            |candidate: &StoredCandidate| replaced.contains(&candidate.item.category_id);
        let shares = shares_blocks(self.candidates.values().filter(|c| in_replaced(c)))
            || shares_blocks(scan.candidates.values());
        self.candidates
            .retain(|_, candidate| !in_replaced(candidate));
        let mut fresh: Vec<Option<FfiCleanCategory>> =
            scan.categories.into_iter().map(Some).collect();
        let mut merged = Vec::with_capacity(self.categories.len() + fresh.len());
        for old in std::mem::take(&mut self.categories) {
            if !replaced.contains(&old.id) {
                merged.push(old);
            } else if let Some(slot) = fresh
                .iter_mut()
                .find(|c| c.as_ref().is_some_and(|c| c.id == old.id))
            {
                merged.extend(slot.take());
            }
        }
        merged.extend(fresh.into_iter().flatten());
        self.categories = merged;
        self.candidates.extend(scan.candidates);
        self.id = scan.id;
        if shares {
            self.keep_completable_shared_blocks();
        }
    }

    /// The merged set, as the caller shows it.
    pub(super) fn record(&self) -> FfiDiscovery {
        FfiDiscovery {
            discovery_id: format!("d{}", self.id),
            categories: self.categories.clone(),
        }
    }

    /// Candidate IDs in display order.
    pub(super) fn candidate_ids(&self) -> impl Iterator<Item = &str> {
        self.categories
            .iter()
            .flat_map(|category| category.candidates.iter().map(|c| c.id.as_str()))
    }

    /// Drops the shared blocks no selection could complete, so a selection's
    /// total counts a block shared across categories once and only when it
    /// holds every link. Report-only candidates are never selected, so theirs
    /// go entirely. Derived from everything seen, over the merged set. Only
    /// the candidates' blocks decide it, so a merge that dropped and added
    /// none with blocks (`shares_blocks`) leaves it as it was.
    fn keep_completable_shared_blocks(&mut self) {
        let mut movable = Vec::new();
        for candidate in self.candidates.values_mut() {
            if matches!(
                candidate.desktop_action,
                FfiDesktopAction::ReportOnly { .. }
            ) {
                candidate.shared = SharedBlocks::default();
            } else {
                candidate.shared = candidate.shared_seen.clone();
                movable.push(&mut candidate.shared);
            }
        }
        SharedBlocks::retain_completable(&mut movable);
    }

    /// Every other candidate at `path` or inside it, selected or not, sorted
    /// by ID. Moving `path` moves them too.
    pub(super) fn covered(&self, id: &str, path: &Path) -> Vec<(&str, &StoredCandidate)> {
        let mut covered: Vec<_> = self
            .candidates
            .iter()
            .filter(|(other, candidate)| *other != id && candidate.item.path.starts_with(path))
            .map(|(other, candidate)| (other.as_str(), candidate))
            .collect();
        covered.sort_unstable_by_key(|(other, _)| *other);
        covered
    }
}

/// Whether any of the candidates saw a shared block.
fn shares_blocks<'a>(mut candidates: impl Iterator<Item = &'a StoredCandidate>) -> bool {
    candidates.any(|candidate| !candidate.shared_seen.is_empty())
}
