//! Running a preview. A preview built by `clean_preview_with_mode` runs
//! through `clean_remove`, which reports deleted, emptied and moved items
//! apart. `clean_execute` and its report stay as they were for previews
//! that only move to the Trash.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use tiny_core::clean::checked_execute::{
    execute_checked, CheckedExecReport, ExecContext, ItemOutcome,
};
use tiny_core::clean::elevated_delete::{ElevatedDelete, ElevatedError, OsascriptDelete};
use tiny_core::clean::finder_trash::Trash;
use tiny_core::clean::fs_safe::{Delete, SafeDelete};
use tiny_core::clean::native_trash::NativeTrash;
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::clean::protection::PolicySnapshot;
use tiny_core::clean::providers::{all_providers_with, CleanProvider};
use tiny_core::clean::trash_plan::{PlannedItem, Removal};
use tiny_core::options::CleanOptions;

use super::protection::protection_error;
use super::{
    app_runner, covers_unselected_review, unselected_review, FfiExecReport, FfiItemResult,
    ProviderFactory, StoredCandidate,
};
use crate::session::CancellationToken;
use crate::{FfiError, ProgressListener, TinySession};

mod report;

use report::removal_report;
pub use report::*;

/// What one run acts through: the real routes, or test fakes.
pub(super) struct Run<'a> {
    providers: ProviderFactory<'a>,
    probe: &'a dyn AppProbe,
    trash: &'a dyn Trash,
    delete: &'a dyn Delete,
    elevated: &'a dyn ElevatedDelete,
    /// `clean_execute`, whose report cannot show a permanent removal.
    trash_only: bool,
}

#[uniffi::export]
impl TinySession {
    /// Runs a preview: each item is moved to the Trash, deleted for good,
    /// pruned or removed by its category's own tool (bounded, never through
    /// a shell or elevated) or, for the Trash row, emptied, as the preview
    /// said (`FfiPreviewItem.permanent`). Consumed before the first change, so
    /// it never runs twice. Every guard of `clean_execute` applies to every
    /// item. Items deleted as administrator (`FfiPreviewItem.needs_admin`) go
    /// first, in one batch behind one macOS authentication dialog; cancelling
    /// it stops the run with nothing changed (`authentication_cancelled`).
    /// Other permanent removals and tools come next, so emptying the Trash
    /// never takes what this run moves there. Moves go through
    /// `NSFileManager` (`NativeTrash`), never Finder, so they need no
    /// Automation permission; only Empty Trash goes through Finder. A
    /// cancellation stops the run; any other failure, a denied Empty Trash
    /// included, stays with its item.
    pub fn clean_remove(
        &self,
        preview_id: String,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiRemovalReport, FfiError> {
        // The only caller of the real dialog route; tests use `remove_clean`.
        if cfg!(test) {
            panic!("a test must run `remove_clean` with fake routes");
        }
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        let providers = |options: &CleanOptions| all_providers_with(options, runner.clone());
        self.remove_clean(
            &preview_id,
            &providers,
            &probe,
            &NativeTrash::default(),
            &SafeDelete,
            &OsascriptDelete,
            &token,
            progress.as_ref(),
        )
    }
}

impl TinySession {
    /// `clean_remove` with injected providers, probe, Trash, deletion and
    /// deletion as administrator (the test seam).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn remove_clean(
        &self,
        preview_id: &str,
        providers: ProviderFactory<'_>,
        probe: &dyn AppProbe,
        trash: &dyn Trash,
        delete: &dyn Delete,
        elevated: &dyn ElevatedDelete,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiRemovalReport, FfiError> {
        let run = Run {
            providers,
            probe,
            trash,
            delete,
            elevated,
            trash_only: false,
        };
        self.run_preview(preview_id, run, token, progress, removal_report)
    }

    /// `clean_execute` with injected providers, probe and Trash (the test
    /// seam).
    pub(crate) fn execute_clean(
        &self,
        preview_id: &str,
        providers: ProviderFactory<'_>,
        probe: &dyn AppProbe,
        trash: &dyn Trash,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiExecReport, FfiError> {
        let run = Run {
            providers,
            probe,
            trash,
            // Never reached: the preview is refused before it is spent.
            delete: &SafeDelete,
            elevated: &NoElevation,
            trash_only: true,
        };
        self.run_preview(preview_id, run, token, progress, exec_report)
    }

    /// Consumes the preview and runs it. The saved rules stay locked until
    /// `report` has built the reply. The lock is taken before the preview is
    /// consumed, so a busy or unreadable store fails without spending the
    /// preview.
    fn run_preview<R>(
        &self,
        preview_id: &str,
        run: Run<'_>,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
        report: fn(&[(String, PlannedItem)], CheckedExecReport) -> R,
    ) -> Result<R, FfiError> {
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        let store = self.protection_store(&home);
        let policy = store.lease().map_err(protection_error)?;
        self.remember_policy(policy.snapshot())
            .map_err(protection_error)?;
        let (items, scanned_with) =
            self.consume_preview(preview_id, policy.snapshot(), run.trash_only)?;
        let providers = providers_scanned_with(run.providers, &scanned_with);
        let plan: Vec<PlannedItem> = items.iter().map(|(_, plan)| plan.clone()).collect();
        let on_progress = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let ctx = ExecContext {
            providers: &providers,
            home: Some(&home),
            probe: run.probe,
            trash: run.trash,
            delete: run.delete,
            elevated: run.elevated,
            cancel: Some(token.flag()),
            progress: Some(&on_progress),
            policy: &policy,
        };
        let executed = execute_checked(&plan, &ctx);
        Ok(report(&items, executed))
    }

    /// Marks the preview consumed, under the lock, before any mutation, once
    /// the saved rules are exactly the ones it was authorized against. A
    /// changed policy drops the preview unspent.
    fn consume_preview(
        &self,
        preview_id: &str,
        policy: &PolicySnapshot,
        trash_only: bool,
    ) -> Result<(Vec<(String, PlannedItem)>, ScannedWith), FfiError> {
        let invalid = |detail: &str| FfiError::PreviewInvalid {
            detail: detail.into(),
        };
        let mut state = self.clean_state();
        let state = &mut *state;
        let discovery = state.discovery.as_ref();
        let preview = state
            .preview
            .as_mut()
            .filter(|p| p.id == preview_id)
            .ok_or_else(|| invalid("unknown or replaced preview"))?;
        if preview.consumed {
            return Err(invalid("preview already used"));
        }
        if trash_only
            && preview
                .items
                .iter()
                .any(|(_, plan)| plan.removal != Removal::MoveToTrash)
        {
            return Err(FfiError::InvalidInput {
                detail: "this preview removes items for good; run it with clean_remove".into(),
            });
        }
        if Instant::now() >= preview.expires_at || SystemTime::now() >= preview.expires_at_wall {
            return Err(invalid("preview expired"));
        }
        let discovery = discovery.ok_or_else(|| invalid("discovery was replaced"))?;
        // A scan replaces the categories it covers and keeps the IDs of the
        // rest, so a preview is stale only when a candidate it selected is
        // gone, or the set of candidates its paths cover changed. Every scan
        // gives the discovery a new ID, so while it is still the one the
        // preview was built on, nothing changed and that pass is skipped.
        let rescanned = discovery.id != preview.discovery_id;
        if rescanned {
            if preview
                .selected
                .iter()
                .any(|id| !discovery.candidates.contains_key(id))
            {
                return Err(invalid("a category was rescanned; scan again"));
            }
            // One pass per item serves the coverage check and PC-C3 below.
            let mut unselected_review = false;
            for (id, plan) in &preview.items {
                let covered = discovery.covered(id, &plan.item.path);
                let unchanged = preview.covered.get(id).is_some_and(|before| {
                    before
                        .iter()
                        .map(String::as_str)
                        .eq(covered.iter().map(|(other, _)| *other))
                });
                if !unchanged {
                    return Err(invalid("a category was rescanned; scan again"));
                }
                unselected_review |= covers_unselected_review(&covered, &preview.selected);
            }
            if unselected_review {
                return Err(invalid("an item would move an unselected review item"));
            }
        } else if preview.items.iter().any(|(id, plan)| {
            !unselected_review(discovery, id, &plan.item.path, &preview.selected).is_empty()
        }) {
            // Defense in depth for PC-C3: preview already excluded these.
            return Err(invalid("an item would move an unselected review item"));
        }
        if preview.policy != *policy {
            state.preview = None;
            return Err(FfiError::ProtectionChanged);
        }
        preview.consumed = true;
        // Every candidate an item carries along is gated by its provider
        // too; the check above made the covered IDs current.
        let scanned_with = scanned_with(
            preview
                .items
                .iter()
                .flat_map(|(id, _)| std::iter::once(id).chain(preview.covered[id].iter()))
                .map(|id| &discovery.candidates[id]),
        );
        Ok((preview.items.clone(), scanned_with))
    }
}

/// The options the run's categories were scanned with, one entry per
/// distinct value, each with the categories scanned that way.
type ScannedWith = Vec<(CleanOptions, BTreeSet<String>)>;

fn scanned_with<'a>(candidates: impl Iterator<Item = &'a StoredCandidate>) -> ScannedWith {
    let mut groups: ScannedWith = Vec::new();
    // A scan's candidates share one `Arc`, so each is compared by value once.
    // The candidates outlive the loop, so a pointer names one `Arc` throughout.
    let mut by_arc: HashMap<*const CleanOptions, usize> = HashMap::new();
    for candidate in candidates {
        let index = *by_arc
            .entry(Arc::as_ptr(&candidate.options))
            .or_insert_with(|| {
                // The category list only chose what the scan covered; no
                // provider is built from it, so scans that differ only there
                // share one build.
                let options = CleanOptions {
                    category: Vec::new(),
                    ..candidate.options.as_ref().clone()
                };
                groups
                    .iter()
                    .position(|(other, _)| *other == options)
                    .unwrap_or_else(|| {
                        groups.push((options, BTreeSet::new()));
                        groups.len() - 1
                    })
            });
        // The executor looks providers up by the item's category.
        let categories = &mut groups[index].1;
        if !categories.contains(&candidate.item.category_id) {
            categories.insert(candidate.item.category_id.clone());
        }
    }
    groups
}

/// The providers of the run's categories, each built from the options its
/// category was scanned with (`docs/internals/clean.md`): one build per
/// distinct options, cut to the categories scanned with them, so a category
/// is never judged by another scan's options.
fn providers_scanned_with(
    factory: ProviderFactory<'_>,
    scanned_with: &ScannedWith,
) -> Vec<Box<dyn CleanProvider>> {
    scanned_with
        .iter()
        .flat_map(|(options, categories)| {
            factory(options)
                .into_iter()
                .filter(|provider| categories.contains(provider.id()))
        })
        .collect()
}

pub(super) fn exec_report(
    items: &[(String, PlannedItem)],
    executed: CheckedExecReport,
) -> FfiExecReport {
    let count = |keep: fn(&ItemOutcome) -> bool| executed.count(keep) as u64;
    let summary = FfiExecReport {
        results: Vec::new(),
        bytes_selected: executed.bytes_selected(),
        bytes_moved_to_trash: executed.bytes_moved_to_trash(),
        moved_count: executed.moved_count() as u64,
        failed_count: count(|o| matches!(o, ItemOutcome::Failed { .. })),
        skipped_count: count(|o| matches!(o, ItemOutcome::Skipped(_))),
        not_attempted_count: count(|o| matches!(o, ItemOutcome::NotAttempted)),
        stopped: executed.stopped.clone().map(Into::into),
    };
    let results = items
        .iter()
        .zip(executed.results)
        .map(|((id, _), result)| FfiItemResult {
            candidate_id: id.clone(),
            category_id: result.item.category_id,
            path: result.item.path.to_string_lossy().into_owned(),
            size_bytes: result.item.size,
            outcome: result.outcome.into(),
        })
        .collect();
    FfiExecReport { results, ..summary }
}

/// `clean_execute` refuses a preview that deletes anything as administrator
/// before spending it, so this never runs; it refuses anyway.
struct NoElevation;

impl ElevatedDelete for NoElevation {
    fn delete(&self, _: &[&Path]) -> Result<(), ElevatedError> {
        Err(ElevatedError::Failed(
            "clean_execute never deletes as administrator".into(),
        ))
    }
}
