//! Running a preview. A preview built by `clean_preview_with_mode` runs
//! through `clean_remove`, which reports deleted, emptied and moved items
//! apart. `clean_execute` and its report stay as they were for previews
//! that only move to the Trash.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use tiny_core::clean::checked_execute::{
    execute_checked, CheckedExecReport, ExecContext, ItemOutcome,
};
use tiny_core::clean::finder_trash::{FinderTrash, Trash, TrashError};
use tiny_core::clean::fs_safe::{Delete, SafeDelete};
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::clean::protection::PolicySnapshot;
use tiny_core::clean::providers::all_providers_with;
use tiny_core::clean::trash_plan::{DeleteMode, PlannedItem, Removal};
use tiny_core::options::CleanOptions;

use super::protection::protection_error;
use super::records::skip_reason;
use super::{
    app_runner, unselected_review, FfiExecReport, FfiItemResult, FfiSkipReason, FfiStopReason,
    ProviderFactory,
};
use crate::session::CancellationToken;
use crate::{FfiError, ProgressListener, TinySession};

/// What a Clean action does with Rebuilt automatically and Project build
/// output, the app's Settings switch (OC1). Your files always go to the
/// Trash and a selected Trash row is always emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDeleteMode {
    /// Move them to the Trash, as `clean_preview` does.
    Trash,
    /// Delete them for good.
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRemovalFailure {
    /// Finder Automation was denied; the run stops.
    AutomationDenied,
    /// Finder could not move the item or empty the Trash.
    TrashFailed,
    /// Permanent deletion failed part way or not at all.
    DeleteFailed,
}

/// How one preview item ended. Nothing is ever retried another way: a failed
/// move is not deleted, a failed deletion is not moved or escalated.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiRemovalOutcome {
    MovedToTrash,
    /// Deleted for good; its bytes are freed.
    Deleted,
    /// The Trash row: Finder emptied the Trash; its bytes are freed.
    TrashEmptied,
    Failed {
        reason: FfiRemovalFailure,
        detail: String,
        /// Re-checked after the failure. A failed deletion may still have
        /// removed part of the item.
        source_still_present: bool,
    },
    Skipped {
        reason: FfiSkipReason,
        /// The guard message, app name, failed check or kept path, when there
        /// is one.
        detail: Option<String>,
    },
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRemovalResult {
    pub candidate_id: String,
    pub category_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub outcome: FfiRemovalOutcome,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRemovalReport {
    /// One entry per preview item, in preview order.
    pub results: Vec<FfiRemovalResult>,
    /// Discovery-time sizes of every preview item.
    pub bytes_selected: u64,
    /// What the deleted items and the emptied Trash freed: their sizes plus
    /// blocks shared only between them, counted once.
    pub bytes_freed: u64,
    /// `bytes_freed` counts blocks the volume could not show as unshared.
    pub bytes_freed_is_upper_bound: bool,
    /// Discovery-time sizes of the items moved to the Trash, which keeps
    /// them on disk: not freed space.
    pub bytes_moved_to_trash: u64,
    /// Items deleted, plus the Trash row when it was emptied.
    pub deleted_count: u64,
    pub moved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub not_attempted_count: u64,
    pub stopped: Option<FfiStopReason>,
}

impl From<FfiDeleteMode> for DeleteMode {
    fn from(mode: FfiDeleteMode) -> Self {
        match mode {
            FfiDeleteMode::Trash => Self::Trash,
            FfiDeleteMode::Permanent => Self::Permanent,
        }
    }
}

/// What one run acts through: the real routes, or test fakes.
pub(super) struct Run<'a> {
    providers: ProviderFactory<'a>,
    probe: &'a dyn AppProbe,
    trash: &'a dyn Trash,
    delete: &'a dyn Delete,
    /// `clean_execute`, whose report cannot show a permanent removal.
    trash_only: bool,
}

#[uniffi::export]
impl TinySession {
    /// Runs a preview: each item is moved to the Trash, deleted for good or,
    /// for the Trash row, emptied, as the preview said
    /// (`FfiPreviewItem.permanent`). Consumed before the first change, so it
    /// never runs twice. Every guard of `clean_execute` applies to every
    /// item. Permanent removals run first, so emptying the Trash never takes
    /// what this run moves there. A Finder Automation denial or cancellation
    /// stops the run; any other failure stays with its item.
    pub fn clean_remove(
        &self,
        preview_id: String,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiRemovalReport, FfiError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        let providers = |options: &CleanOptions| all_providers_with(options, runner.clone());
        self.remove_clean(
            &preview_id,
            &providers,
            &probe,
            &FinderTrash,
            &SafeDelete,
            &token,
            progress.as_ref(),
        )
    }
}

impl TinySession {
    /// `clean_remove` with injected providers, probe, Trash and deletion
    /// (the test seam).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn remove_clean(
        &self,
        preview_id: &str,
        providers: ProviderFactory<'_>,
        probe: &dyn AppProbe,
        trash: &dyn Trash,
        delete: &dyn Delete,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiRemovalReport, FfiError> {
        let run = Run {
            providers,
            probe,
            trash,
            delete,
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
        let (items, options) =
            self.consume_preview(preview_id, policy.snapshot(), run.trash_only)?;
        let providers = (run.providers)(&options);
        let plan: Vec<PlannedItem> = items.iter().map(|(_, plan)| plan.clone()).collect();
        let on_progress = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let ctx = ExecContext {
            providers: &providers,
            home: Some(&home),
            probe: run.probe,
            trash: run.trash,
            delete: run.delete,
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
    ) -> Result<(Vec<(String, PlannedItem)>, CleanOptions), FfiError> {
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
        let discovery = discovery
            .filter(|d| d.id == preview.discovery_id)
            .ok_or_else(|| invalid("discovery was replaced"))?;
        // Defense in depth for PC-C3: preview already excluded these.
        if preview.items.iter().any(|(id, plan)| {
            !unselected_review(discovery, id, &plan.item.path, &preview.selected).is_empty()
        }) {
            return Err(invalid("an item would move an unselected review item"));
        }
        if preview.policy != *policy {
            state.preview = None;
            return Err(FfiError::ProtectionChanged);
        }
        preview.consumed = true;
        Ok((preview.items.clone(), discovery.options.clone()))
    }
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

fn removal_report(
    items: &[(String, PlannedItem)],
    executed: CheckedExecReport,
) -> FfiRemovalReport {
    let count = |keep: fn(&ItemOutcome) -> bool| executed.count(keep) as u64;
    let summary = FfiRemovalReport {
        results: Vec::new(),
        bytes_selected: executed.bytes_selected(),
        bytes_freed: executed.bytes_freed,
        bytes_freed_is_upper_bound: executed.freed_is_upper_bound,
        bytes_moved_to_trash: executed.bytes_moved_to_trash(),
        deleted_count: count(ItemOutcome::frees_space),
        moved_count: executed.moved_count() as u64,
        failed_count: count(|o| {
            matches!(
                o,
                ItemOutcome::Failed { .. } | ItemOutcome::DeleteFailed { .. }
            )
        }),
        skipped_count: count(|o| matches!(o, ItemOutcome::Skipped(_))),
        not_attempted_count: count(|o| matches!(o, ItemOutcome::NotAttempted)),
        stopped: executed.stopped.clone().map(Into::into),
    };
    let results = items
        .iter()
        .zip(executed.results)
        .map(|((id, _), result)| FfiRemovalResult {
            candidate_id: id.clone(),
            category_id: result.item.category_id,
            path: result.item.path.to_string_lossy().into_owned(),
            size_bytes: result.item.size,
            outcome: result.outcome.into(),
        })
        .collect();
    FfiRemovalReport { results, ..summary }
}

impl From<ItemOutcome> for FfiRemovalOutcome {
    fn from(outcome: ItemOutcome) -> Self {
        match outcome {
            ItemOutcome::MovedToTrash => Self::MovedToTrash,
            ItemOutcome::Deleted => Self::Deleted,
            ItemOutcome::TrashEmptied => Self::TrashEmptied,
            ItemOutcome::Failed {
                error,
                source_present,
            } => {
                let (reason, detail) = match error {
                    TrashError::AutomationDenied(d) => (FfiRemovalFailure::AutomationDenied, d),
                    TrashError::Failed(d) => (FfiRemovalFailure::TrashFailed, d),
                };
                Self::Failed {
                    reason,
                    detail,
                    source_still_present: source_present,
                }
            }
            ItemOutcome::DeleteFailed {
                detail,
                source_present,
            } => Self::Failed {
                reason: FfiRemovalFailure::DeleteFailed,
                detail,
                source_still_present: source_present,
            },
            ItemOutcome::Skipped(reason) => {
                let (reason, detail) = skip_reason(reason);
                Self::Skipped { reason, detail }
            }
            ItemOutcome::NotAttempted => Self::NotAttempted,
        }
    }
}
