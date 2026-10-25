//! Installed-app inventory, trusted preview and checked Trash execution for
//! the native uninstall flow.
//!
//! The session keeps the listing (app IDs) and one preview (a plan frozen by
//! core with item IDs). Swift names an app ID, then a preview ID plus item IDs,
//! never a path. Execution runs core's `execute_checked_uninstall` through the
//! `Trash` seam only: no hard delete, no tool command, no replay.
//!
//! Locking: session gate, then the saved-rules lease, then a short
//! uninstall-state lock that never spans a probe, Trash call or callback.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use tiny_core::clean::checked_execute::ItemOutcome;
use tiny_core::clean::finder_trash::{FinderTrash, Trash};
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::clean::protection::{Matcher, PolicySnapshot};
use tiny_core::options::UninstallOptions;
use tiny_core::uninstall::{
    blocked_reason, build_checked_plan, build_plan_in, doubt_reason, execute_checked_uninstall,
    valid_bundle_id, AppEntry, CheckedPlan, Consent, ItemKind, UninstallContext, UninstallReport,
};

use crate::clean::{app_runner, guards, protection_error, FfiProtectionGuard, PREVIEW_TTL};
use crate::session::CancellationToken;
use crate::{FfiError, ProgressListener, TinySession};

pub(crate) mod catalog;
mod records;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use records::*;

use catalog::{AppCatalog, SystemCatalog};

/// Session-held uninstall state.
#[derive(Default)]
pub(crate) struct UninstallState {
    next_id: u64,
    listing: Option<StoredListing>,
    preview: Option<StoredPreview>,
    /// Test override for `PREVIEW_TTL`.
    ttl: Option<Duration>,
}

struct StoredListing {
    id: u64,
    apps: HashMap<String, AppEntry>,
}

struct StoredPreview {
    id: String,
    listing_id: u64,
    app: AppEntry,
    plan: CheckedPlan,
    /// Item IDs by plan index.
    item_ids: Vec<String>,
    /// Whether each item (by plan index) may be selected.
    selectable: Vec<bool>,
    /// Both clocks, as for Clean previews: `Instant` ignores wall-clock
    /// changes, `SystemTime` keeps running while the Mac sleeps.
    expires_at: Instant,
    expires_at_wall: SystemTime,
    /// Kept after use so a replay reports "already used", not "unknown".
    consumed: bool,
    /// The saved rules this preview was authorized against.
    policy: PolicySnapshot,
}

/// What `consume_preview` hands to execution.
struct Run {
    app: AppEntry,
    plan: CheckedPlan,
    item_ids: Vec<String>,
    selection: BTreeSet<usize>,
}

/// Everything execution touches besides the session: injected in tests.
pub(crate) struct Seams<'a> {
    pub(crate) catalog: &'a dyn AppCatalog,
    pub(crate) probe: &'a dyn AppProbe,
    pub(crate) trash: &'a dyn Trash,
}

#[uniffi::export]
impl TinySession {
    /// Lists the installed apps. Holds the operation gate (it walks every
    /// bundle for its size), so it returns `Busy` while a scan or mutation
    /// runs. Replaces the previous listing and any preview.
    pub fn uninstall_list_apps(&self) -> Result<Vec<FfiInstalledApp>, FfiError> {
        self.list_installed_apps(&SystemCatalog)
    }

    /// Builds a preview for one app of the current listing: the bundle and
    /// its leftovers as core classifies them, frozen for execution. Replaces
    /// any earlier preview. Reads the saved "always keep" rules; unreadable
    /// rules fail it (`ProtectionUnavailable`).
    pub fn uninstall_preview(&self, app_id: String) -> Result<FfiUninstallPreview, FfiError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.preview_uninstall(&app_id, &SystemCatalog, &probe)
    }

    /// Moves the selected items of a preview to Trash through Finder. The
    /// preview is spent before the first check that can refuse for
    /// app state, so it never runs twice; an unknown item, an empty or
    /// selection without the app bundle, an item that is report-only,
    /// uncertain or held (all `InvalidInput`) and a changed rule set are
    /// refused first.
    /// Every outcome after execution starts is in the returned report;
    /// a Finder Automation denial or cancellation stops it.
    pub fn uninstall_execute(
        &self,
        preview_id: String,
        selected_item_ids: Vec<String>,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiUninstallReport, FfiError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        let seams = Seams {
            catalog: &SystemCatalog,
            probe: &probe,
            trash: &FinderTrash,
        };
        self.execute_uninstall(
            &preview_id,
            &selected_item_ids,
            &seams,
            &token,
            progress.as_ref(),
        )
    }
}

impl UninstallState {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

impl TinySession {
    fn uninstall_state(&self) -> MutexGuard<'_, UninstallState> {
        self.uninstall
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// `uninstall_list_apps` with an injected inventory (the test seam).
    pub(crate) fn list_installed_apps(
        &self,
        catalog: &dyn AppCatalog,
    ) -> Result<Vec<FfiInstalledApp>, FfiError> {
        let _gate = self.begin()?;
        {
            self.uninstall_state().preview = None;
        }
        let mut apps = catalog.list()?;
        apps.sort_by_key(|app| (app.name.to_lowercase(), app.path.clone()));
        let mut state = self.uninstall_state();
        let id = state.next_id();
        let mut stored = HashMap::new();
        let listed = apps
            .into_iter()
            .enumerate()
            .map(|(index, app)| {
                let app_id = format!("a{id}-{index}");
                let record = installed_app(&app_id, &app);
                stored.insert(app_id, app);
                record
            })
            .collect();
        state.listing = Some(StoredListing { id, apps: stored });
        Ok(listed)
    }

    /// `uninstall_preview` with an injected inventory and probe (the test seam).
    pub(crate) fn preview_uninstall(
        &self,
        app_id: &str,
        catalog: &dyn AppCatalog,
        probe: &dyn AppProbe,
    ) -> Result<FfiUninstallPreview, FfiError> {
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        let (listing_id, listed) = self.listed_app(app_id)?;
        // Identity is read again: the listing may be old.
        let inventory = catalog.identities().ok();
        let app = current_entry(listed, inventory.as_deref())?;
        let opts = UninstallOptions {
            shallow: false,
            leftovers_only: false,
            force: false,
        };
        let plan = build_plan_in(&app, &opts, inventory.as_deref(), Some(&home))?;
        let executable = catalog.executable(&app.path);
        let checked = build_checked_plan(&plan, &home, catalog.applications_root(), executable)
            .map_err(|r| refused(records::refusal(r, |i| i.to_string())))?;
        // Rules are read without the state lock (lock order).
        let policy = self.load_policy()?;
        let run_state = run_state(probe, checked.executable());
        let (leftovers_searched, leftovers_skipped_reason) = leftover_search(&app);
        let mut state = self.uninstall_state();
        let ttl = state.ttl.unwrap_or(PREVIEW_TTL);
        let preview_id = format!("u{}", state.next_id());
        let item_ids: Vec<String> = (0..checked.items().len())
            .map(|index| format!("{preview_id}-{index}"))
            .collect();
        let items = preview_items(&checked, &item_ids, &policy);
        let selectable: Vec<bool> = items.iter().map(|item| item.selectable).collect();
        let selectable_bytes = items
            .iter()
            .filter(|item| item.selectable)
            .fold(0u64, |sum, item| sum.saturating_add(item.size_bytes));
        let expires_at = SystemTime::now() + ttl;
        let preview = FfiUninstallPreview {
            preview_id: preview_id.clone(),
            app_id: app_id.to_string(),
            app_name: app.name.clone(),
            items,
            run_state,
            bundle_id: app.bundle_id.clone(),
            leftovers_searched,
            leftovers_skipped_reason,
            selectable_bytes,
            expires_in_seconds: ttl.as_secs(),
            expires_at: unix_seconds(expires_at),
        };
        state.preview = Some(StoredPreview {
            id: preview_id,
            listing_id,
            app,
            plan: checked,
            item_ids,
            selectable,
            expires_at: Instant::now() + ttl,
            expires_at_wall: expires_at,
            consumed: false,
            policy,
        });
        Ok(preview)
    }

    /// `uninstall_execute` with injected seams (the test seam). The saved
    /// rules stay locked until the report is built, and the lease is taken
    /// before the preview is consumed, so a busy or unreadable store fails
    /// without spending it.
    pub(crate) fn execute_uninstall(
        &self,
        preview_id: &str,
        selected_item_ids: &[String],
        seams: &Seams<'_>,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiUninstallReport, FfiError> {
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        let store = self.protection_store(&home);
        let policy = store.lease().map_err(protection_error)?;
        self.remember_policy(policy.snapshot())
            .map_err(protection_error)?;
        let run =
            self.consume_uninstall_preview(preview_id, selected_item_ids, policy.snapshot())?;
        let ids = |index: usize| {
            run.item_ids
                .get(index)
                .cloned()
                .unwrap_or_else(|| index.to_string())
        };
        recheck_twin(&run, seams.catalog)?;
        let report = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let ctx = UninstallContext {
            home: &home,
            probe: seams.probe,
            trash: seams.trash,
            cancel: Some(token.flag()),
            progress: Some(&report),
            policy: &policy,
        };
        let executed = execute_checked_uninstall(&run.plan, &run.selection, &ctx)
            .map_err(|r| refused(records::refusal(r, ids)))?;
        Ok(exec_report(&run.item_ids, executed))
    }

    /// The current listing's ID and the app stored under `app_id`.
    fn listed_app(&self, app_id: &str) -> Result<(u64, AppEntry), FfiError> {
        let state = self.uninstall_state();
        let listing = state
            .listing
            .as_ref()
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: "no current app list; list the apps again".into(),
            })?;
        let app = listing
            .apps
            .get(app_id)
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: format!("unknown app {app_id}; list the apps again"),
            })?;
        Ok((listing.id, app.clone()))
    }

    /// Resolves the selection, then marks the preview consumed under the lock
    /// before any mutation, once the saved rules are exactly the ones it was
    /// authorized against (a changed rule set drops it unspent). A bad
    /// selection is refused without spending the preview.
    fn consume_uninstall_preview(
        &self,
        preview_id: &str,
        selected_item_ids: &[String],
        policy: &PolicySnapshot,
    ) -> Result<Run, FfiError> {
        let invalid = |detail: &str| FfiError::PreviewInvalid {
            detail: detail.into(),
        };
        let mut state = self.uninstall_state();
        let state = &mut *state;
        let current_listing = state.listing.as_ref().map(|l| l.id);
        let preview = state
            .preview
            .as_mut()
            .filter(|p| p.id == preview_id)
            .ok_or_else(|| invalid("unknown or replaced preview"))?;
        if preview.consumed {
            return Err(invalid("preview already used"));
        }
        if Instant::now() >= preview.expires_at || SystemTime::now() >= preview.expires_at_wall {
            return Err(invalid("preview expired"));
        }
        if current_listing != Some(preview.listing_id) {
            return Err(invalid("the app list was replaced"));
        }
        let selection = resolve_selection(preview, selected_item_ids)?;
        if preview.policy != *policy {
            state.preview = None;
            return Err(FfiError::ProtectionChanged);
        }
        preview.consumed = true;
        Ok(Run {
            app: preview.app.clone(),
            plan: preview.plan.clone(),
            item_ids: preview.item_ids.clone(),
            selection,
        })
    }
}

/// Item IDs to plan indexes. Everything a caller can get wrong is refused
/// here as `InvalidInput`, before the preview is spent: an unknown ID, an
/// item that is report-only, uncertain or held by a saved rule, an empty
/// selection, and a selection without the app bundle.
fn resolve_selection(
    preview: &StoredPreview,
    selected_item_ids: &[String],
) -> Result<BTreeSet<usize>, FfiError> {
    let invalid = |detail: String| FfiError::InvalidInput { detail };
    let mut selection = BTreeSet::new();
    for id in selected_item_ids {
        let index = preview
            .item_ids
            .iter()
            .position(|known| known == id)
            .ok_or_else(|| invalid(format!("unknown item {id}")))?;
        if !preview.selectable[index] {
            let ownership = FfiUninstallOwnership::from(preview.plan.items()[index].ownership());
            return Err(invalid(match ownership {
                FfiUninstallOwnership::ReportOnly { reason }
                | FfiUninstallOwnership::Uncertain { reason } => {
                    format!("item {id} cannot be removed: {reason}")
                }
                _ => format!("item {id} is held by a saved rule"),
            }));
        }
        selection.insert(index);
    }
    if selection.is_empty() {
        return Err(invalid("nothing is selected".into()));
    }
    let has_bundle = selection
        .iter()
        .any(|&i| preview.plan.items()[i].kind() == ItemKind::AppBundle);
    if !has_bundle {
        return Err(invalid("select the app to uninstall it".into()));
    }
    Ok(selection)
}

/// O5 again, with a fresh inventory: leftovers were classified as owned when
/// no other copy of the app existed. Another copy installed since (or an
/// unreadable inventory) refuses the run. A bundle-only run touches nothing
/// the copies share, so it is not rechecked.
fn recheck_twin(run: &Run, catalog: &dyn AppCatalog) -> Result<(), FfiError> {
    let leftover_selected = run
        .selection
        .iter()
        .any(|&i| run.plan.items()[i].kind() == ItemKind::Leftover);
    let Some(id) = run.app.bundle_id.as_deref().filter(|_| leftover_selected) else {
        return Ok(());
    };
    let inventory = catalog.identities().ok();
    match doubt_reason(&run.app.path, id, inventory.as_deref()) {
        None => Ok(()),
        Some(_) if inventory.is_none() => Err(refused(FfiUninstallRefusal::InventoryUnavailable)),
        Some(_) => Err(refused(FfiUninstallRefusal::DuplicateBundleId)),
    }
}

fn refused(refusal: FfiUninstallRefusal) -> FfiError {
    FfiError::UninstallRefused { refusal }
}

/// The listed entry with its identity read again. With no inventory the
/// listed identity stands and every leftover becomes report-only (O5).
fn current_entry(listed: AppEntry, inventory: Option<&[AppEntry]>) -> Result<AppEntry, FfiError> {
    let Some(inventory) = inventory else {
        return Ok(listed);
    };
    let fresh = inventory
        .iter()
        .find(|entry| entry.path == listed.path)
        .ok_or_else(|| FfiError::PreviewInvalid {
            detail: format!(
                "{} is no longer installed; list the apps again",
                listed.name
            ),
        })?;
    Ok(AppEntry {
        bundle_id: fresh.bundle_id.clone(),
        ..listed
    })
}

fn installed_app(app_id: &str, app: &AppEntry) -> FfiInstalledApp {
    FfiInstalledApp {
        app_id: app_id.to_string(),
        name: app.name.clone(),
        bundle_id: app.bundle_id.clone(),
        path: lossy(&app.path),
        size_bytes: app.size,
        last_used_days: app.last_used_days,
        blocked_reason: blocked_reason(app, false),
    }
}

/// O1: leftovers are looked up only for a valid reverse-DNS bundle id.
fn leftover_search(app: &AppEntry) -> (bool, Option<String>) {
    match app.bundle_id.as_deref() {
        None => (
            false,
            Some("the app's bundle identifier could not be read".into()),
        ),
        Some(id) if !valid_bundle_id(id) => (
            false,
            Some("the app's bundle identifier is not a valid reverse-DNS name".into()),
        ),
        Some(_) => (true, None),
    }
}

fn run_state(probe: &dyn AppProbe, executable: Option<&str>) -> FfiAppRunState {
    let Some(name) = executable else {
        return FfiAppRunState::Unknown {
            detail: "the app's process name could not be read".into(),
        };
    };
    match probe.probe(name) {
        Ok(true) => FfiAppRunState::Running,
        Ok(false) => FfiAppRunState::NotRunning,
        Err(detail) => FfiAppRunState::Unknown { detail },
    }
}

fn preview_items(
    plan: &CheckedPlan,
    item_ids: &[String],
    policy: &PolicySnapshot,
) -> Vec<FfiUninstallItem> {
    let matcher = Matcher::new();
    plan.items()
        .iter()
        .zip(item_ids)
        .map(|(item, item_id)| {
            let held = held_by(policy, &matcher, item.path());
            FfiUninstallItem {
                item_id: item_id.clone(),
                path: lossy(item.path()),
                size_bytes: item.size(),
                kind: item.kind().into(),
                ownership: item.ownership().into(),
                selectable: item.ownership().allows(Consent::Explicit) && held.is_none(),
                held_by: held,
            }
        })
        .collect()
}

/// The strongest saved rule holding `path`, if any.
fn held_by(policy: &PolicySnapshot, matcher: &Matcher, path: &Path) -> Option<FfiProtectionGuard> {
    guards(policy, &policy.evaluate(matcher, path))
        .into_iter()
        .next()
}

fn exec_report(item_ids: &[String], executed: UninstallReport) -> FfiUninstallReport {
    let count = |keep: fn(&ItemOutcome) -> bool| {
        executed.results.iter().filter(|r| keep(&r.outcome)).count() as u64
    };
    let bytes_selected = executed
        .results
        .iter()
        .fold(0, |sum, r| u64::saturating_add(sum, r.size));
    let summary = FfiUninstallReport {
        results: Vec::new(),
        bytes_selected,
        bytes_moved_to_trash: executed.bytes_moved_to_trash(),
        moved_count: executed.moved_count() as u64,
        failed_count: count(|o| matches!(o, ItemOutcome::Failed { .. })),
        skipped_count: count(|o| matches!(o, ItemOutcome::Skipped(_))),
        not_attempted_count: count(|o| matches!(o, ItemOutcome::NotAttempted)),
        stopped: executed.stopped.clone().map(Into::into),
    };
    let results = executed
        .results
        .into_iter()
        .map(|result| FfiUninstallItemResult {
            item_id: item_ids[result.index].clone(),
            path: lossy(&result.path),
            kind: result.kind.into(),
            size_bytes: result.size,
            outcome: result.outcome.into(),
        })
        .collect();
    FfiUninstallReport { results, ..summary }
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(i64::MAX)
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
