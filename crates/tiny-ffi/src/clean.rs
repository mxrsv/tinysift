//! Cleanup discovery, trusted preview and checked execution for the native
//! app.
//!
//! The session keeps what discovery found (paths, fingerprints, roots and
//! the options used). A preview is built only from those candidate IDs, and
//! execution only from a preview ID, so Swift never hands Rust a path to act
//! on. Execution moves each path through the core `Trash` route, deletes it
//! through `fs_safe`, runs the category's own tool command, or empties the
//! Trash through Finder, as the preview decided; it never calls a provider's
//! `execute`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use tiny_core::clean::discover::{
    discover_checked, select_providers_with, validate_options, CategoryOutcome, CheckedCategory,
};
use tiny_core::clean::finder_trash::FinderTrash;
use tiny_core::clean::freed_size::SharedBlocks;
use tiny_core::clean::fs_safe::PathFingerprint;
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::clean::protection::{PolicySnapshot, RuleScope};
use tiny_core::clean::protection_store::ProtectionStore;
use tiny_core::clean::providers::{
    all_providers_with, category_family, category_section, comes_back, desktop_report_only,
    known_category_ids, CleanProvider, DesktopTool, ReportOnly,
};
use tiny_core::clean::scan_context::{ScanContext, Unreadable};
use tiny_core::clean::trash_plan::{
    account_home, freed_together, partition_overlaps, removal_for, trusted_home, DeleteMode,
    Overlap, PlannedItem, Removal,
};
use tiny_core::clean::types::{CleanItem, RiskLevel};
use tiny_core::options::CleanOptions;
use tiny_core::runner::{CommandRunner, ToolLookup, ToolRunner};

use crate::session::CancellationToken;
use crate::{FfiError, ProgressListener, TinySession};

mod protection;
mod records;
mod removal;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use protection::*;
pub use records::*;
pub use removal::*;

use protection::{candidate_scope, split_protected};

/// How long a confirmed preview stays executable.
pub const PREVIEW_TTL: Duration = Duration::from_secs(15 * 60);

/// Session-held cleanup state. Never locked across provider work, Trash
/// calls or progress callbacks.
#[derive(Default)]
pub(crate) struct CleanState {
    next_id: u64,
    discovery: Option<StoredDiscovery>,
    preview: Option<StoredPreview>,
    /// Test override for `PREVIEW_TTL`.
    ttl: Option<Duration>,
    /// Test override for (`HOME`, the account home) fed to `trusted_home`.
    home_inputs: Option<(Option<PathBuf>, Option<PathBuf>)>,
    /// Test override for the protection store directory. A test that
    /// executes must set it; see `TinySession::protection_store`.
    protection_dir: Option<PathBuf>,
    /// The saved rules `clean_protection` or the last write handed to Swift.
    /// A write needs the store to still equal them in revision and content.
    seen_policy: Option<PolicySnapshot>,
    /// Sticky across read/write failures and rescans: lost settings are never first use.
    stored_policy_seen: bool,
}

struct StoredDiscovery {
    id: u64,
    options: CleanOptions,
    candidates: HashMap<String, StoredCandidate>,
}

struct StoredCandidate {
    item: CleanItem,
    /// `None` when the path could not be stat'ed (e.g. a tool placeholder).
    fingerprint: Option<PathFingerprint>,
    roots: Arc<Vec<PathBuf>>,
    desktop_action: FfiDesktopAction,
    /// What "Always keep" would cover, judged once from the path's own metadata.
    keep_scope: Option<RuleScope>,
    size_is_upper_bound: bool,
    /// Only what other movable candidates could complete
    /// (`SharedBlocks::retain_completable`).
    shared: SharedBlocks,
    /// The category's own tool, when the app asked for tool commands.
    tool: Option<DesktopTool>,
}

struct StoredPreview {
    id: String,
    discovery_id: u64,
    /// `(candidate ID, plan)` in execution order.
    items: Vec<(String, PlannedItem)>,
    /// Both clocks: `Instant` ignores wall-clock changes, `SystemTime`
    /// keeps running while the Mac sleeps, which `Instant` does not.
    expires_at: Instant,
    expires_at_wall: SystemTime,
    /// Kept after use so a replay reports "consumed", not "unknown".
    consumed: bool,
    /// Candidate IDs the user selected, for the execute-time PC-C3 check.
    selected: BTreeSet<String>,
    /// The saved rules this preview was authorized against; execution needs
    /// them unchanged, in revision and content.
    policy: PolicySnapshot,
}

impl CleanState {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

#[uniffi::export]
impl TinySession {
    /// Scans cleanup categories. Holds the operation gate, so it returns
    /// `Busy` while another scan or mutation runs. Replaces the previous
    /// discovery and any preview built from it.
    pub fn clean_discover(
        &self,
        options: FfiCleanOptions,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiDiscovery, FfiError> {
        let options = CleanOptions::from(options);
        let runner = app_runner();
        let providers = select_providers_with(&options, runner.clone());
        let probe = RunnerProbe(runner.as_ref());
        self.discover_clean(options, &providers, &probe, &token, progress.as_ref())
    }

    /// The categories `clean_discover` would scan with these options, in scan
    /// order, without scanning or taking the operation gate. A scan may omit
    /// one whose tool or folder is absent on this Mac, so this is the most it
    /// can return, not what it will find.
    pub fn clean_places(&self, options: FfiCleanOptions) -> Vec<FfiCleanPlace> {
        let options = CleanOptions::from(options);
        select_providers_with(&options, app_runner())
            .iter()
            .map(|p| FfiCleanPlace {
                id: p.id().to_string(),
                label: p.label().to_string(),
            })
            .collect()
    }

    /// Builds a preview from candidate IDs of the current discovery. Unknown
    /// or stale IDs are rejected; paths the saved "always keep" rules hold,
    /// and overlapping paths, are listed as excluded. Unreadable saved rules
    /// fail it (`ProtectionUnavailable`) rather than authorize against
    /// unknown rules. Replaces any earlier preview. Every item moves to the
    /// Trash, except a selected Trash row, which is emptied.
    pub fn clean_preview(&self, candidate_ids: Vec<String>) -> Result<FfiPreview, FfiError> {
        self.preview_clean(candidate_ids, DeleteMode::Trash)
    }

    /// `clean_preview` for the app's delete mode: with `Permanent`, items in
    /// Rebuilt automatically and Project build output are deleted for good,
    /// unless they would carry along a path that only goes to the Trash, and
    /// a uv or pnpm cache is pruned by its tool. Your files always go to the
    /// Trash; a simulator runtime's removal is requested in either mode. Run
    /// it with `clean_remove`.
    pub fn clean_preview_with_mode(
        &self,
        candidate_ids: Vec<String>,
        mode: FfiDeleteMode,
    ) -> Result<FfiPreview, FfiError> {
        self.preview_clean(candidate_ids, mode.into())
    }

    /// Moves the preview's paths to Trash through Finder. The preview is
    /// consumed before the first move, so it can never run twice, even
    /// after a panic. Always returns the per-item report once execution
    /// starts; a Finder Automation denial stops it with
    /// `stopped = AutomationDenied`. A preview that removes anything for good
    /// is refused unspent (`InvalidInput`): run it with `clean_remove`.
    pub fn clean_execute(
        &self,
        preview_id: String,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiExecReport, FfiError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        let providers = |options: &CleanOptions| all_providers_with(options, runner.clone());
        // The real saved rules: execution never runs without them.
        self.execute_clean(
            &preview_id,
            &providers,
            &probe,
            &FinderTrash,
            &token,
            progress.as_ref(),
        )
    }
}

impl TinySession {
    fn preview_clean(
        &self,
        candidate_ids: Vec<String>,
        mode: DeleteMode,
    ) -> Result<FfiPreview, FfiError> {
        let selected: BTreeSet<String> = candidate_ids.iter().cloned().collect();
        let (discovery_id, resolved) = self.resolve_selection(candidate_ids)?;
        // Rules and path comparison run without the state lock (lock order).
        let policy = self.load_policy()?;
        let (allowed, mut excluded) = split_protected(&policy, resolved);
        let mut state = self.clean_state();
        let ttl = state.ttl.unwrap_or(PREVIEW_TTL);
        let discovery = state
            .discovery
            .as_ref()
            .filter(|d| d.id == discovery_id)
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: "discovery was replaced; scan again".into(),
            })?;
        let mut planned = Vec::new();
        // PC-C3 before overlaps: a blocked ancestor must not hide its selected children.
        for (id, plan) in allowed {
            let review = unselected_review(discovery, &id, &plan.item.path, &selected);
            if review.is_empty() {
                planned.push((id, plan));
            } else {
                excluded.push(FfiPreviewExclusion {
                    candidate_id: id,
                    path: plan.item.path.to_string_lossy().into_owned(),
                    reason: FfiExclusionReason::CoversUnselectedReview {
                        candidate_ids: review,
                    },
                });
            }
        }
        let (mut kept, covered) = partition_overlaps(planned, |(_, plan)| plan.item.path.as_path());
        for (id, plan) in &mut kept {
            plan.covers = covered_candidates(discovery, id, &plan.item.path);
            let candidate = discovery.candidates.get(id.as_str());
            plan.removal = match candidate.map(|c| c.desktop_action) {
                Some(FfiDesktopAction::EmptyTrash) => Removal::EmptyTrash,
                Some(FfiDesktopAction::DeleteAsAdministrator) => Removal::DeleteAsAdministrator,
                _ => removal_for(
                    &plan.item,
                    &plan.covers,
                    mode,
                    candidate.and_then(|c| c.tool),
                ),
            };
        }
        excluded.extend(
            covered
                .into_iter()
                .map(|((id, plan), overlap)| exclusion(&kept, id, &plan, overlap)),
        );
        let items = kept
            .iter()
            .map(|(id, plan)| preview_item(discovery, &selected, id, plan))
            .collect();
        let plans = || kept.iter().map(|(_, plan)| plan);
        let with = |removal: Removal| move |plan: &&PlannedItem| plan.removal == removal;
        // A requested removal is neither freed nor moved when the run ends.
        let frees = |plan: &&PlannedItem| {
            matches!(
                plan.removal,
                Removal::Delete
                    | Removal::DeleteAsAdministrator
                    | Removal::EmptyTrash
                    | Removal::RunTool(DesktopTool::Prune)
            )
        };
        let (bytes_to_free, bytes_to_free_is_upper_bound) = freed_together(plans().filter(frees));
        let request = Removal::RunTool(DesktopTool::Request);
        let as_admin = Removal::DeleteAsAdministrator;
        let preview_id = format!("p{}", state.next_id());
        let preview = FfiPreview {
            preview_id: preview_id.clone(),
            items,
            excluded,
            bytes_selected: total_size(plans().map(|plan| &plan.item)),
            expires_in_seconds: ttl.as_secs(),
            bytes_to_free,
            bytes_to_free_is_upper_bound,
            bytes_to_trash: total_size(plans().filter(with(Removal::MoveToTrash)).map(|p| &p.item)),
            bytes_requested: total_size(plans().filter(with(request)).map(|p| &p.item)),
            bytes_needing_admin: total_size(plans().filter(with(as_admin)).map(|p| &p.item)),
        };
        state.preview = Some(StoredPreview {
            id: preview_id,
            discovery_id,
            items: kept,
            expires_at: Instant::now() + ttl,
            expires_at_wall: SystemTime::now() + ttl,
            consumed: false,
            selected,
            policy,
        });
        Ok(preview)
    }
}

/// The app's runner: `PATH`, then the Homebrew prefixes.
pub(crate) fn app_runner() -> Arc<dyn CommandRunner> {
    Arc::new(ToolRunner::new(ToolLookup::app()))
}

type ProviderFactory<'a> = &'a dyn Fn(&CleanOptions) -> Vec<Box<dyn CleanProvider>>;

#[cfg(test)]
impl TinySession {
    /// Points the session at a temp (`HOME`, account home) pair and a temp
    /// saved-rules store, for tests of features that share Clean's home and
    /// rules. A trusted home passes the same path twice.
    pub(crate) fn inject_clean_inputs(&self, env: &Path, account: &Path, rules: PathBuf) {
        let mut state = self.clean_state();
        state.home_inputs = Some((Some(env.to_path_buf()), Some(account.to_path_buf())));
        state.protection_dir = Some(rules);
    }
}

impl TinySession {
    /// Lock on cleanup state. Locked sections never run foreign or provider
    /// code, so a poisoned lock is recovered rather than failing every call.
    pub(crate) fn clean_state(&self) -> MutexGuard<'_, CleanState> {
        self.clean.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current discovery's ID and the plan for `candidate_ids`, from the
    /// stored candidates only. Short lock.
    fn resolve_selection(
        &self,
        candidate_ids: Vec<String>,
    ) -> Result<(u64, Vec<(String, PlannedItem)>), FfiError> {
        let state = self.clean_state();
        let discovery = state
            .discovery
            .as_ref()
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: "no current discovery; scan again".into(),
            })?;
        Ok((discovery.id, plan_selection(discovery, candidate_ids)?))
    }

    /// `clean_discover` with injected providers and probe (the test seam).
    pub(crate) fn discover_clean(
        &self,
        options: CleanOptions,
        providers: &[Box<dyn CleanProvider>],
        probe: &dyn AppProbe,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiDiscovery, FfiError> {
        validate_options(&options)?;
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        {
            let mut state = self.clean_state();
            state.discovery = None;
            state.preview = None;
        }
        let report = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let ctx = ScanContext::new(Some(token.flag()), probe).refusing_protected_paths(home);
        let checked = discover_checked(providers, &ctx, Some(&report));
        if checked.cancelled {
            return Err(FfiError::Cancelled);
        }
        let mut state = self.clean_state();
        let id = state.next_id();
        let mut candidates = HashMap::new();
        let categories = checked
            .categories
            .into_iter()
            .map(|category| {
                let (action, tool) = desktop_action(providers, &category.id, &options);
                let needs_admin = action == FfiDesktopAction::DeleteAsAdministrator;
                store_category(id, category, action, tool, needs_admin, &mut candidates)
            })
            .collect();
        keep_completable_shared_blocks(&mut candidates);
        state.discovery = Some(StoredDiscovery {
            id,
            options,
            candidates,
        });
        Ok(FfiDiscovery {
            discovery_id: format!("d{id}"),
            categories,
        })
    }

    /// `HOME` checked against the account home before any scan or move.
    pub(crate) fn clean_home(&self) -> Result<PathBuf, FfiError> {
        let (env, account) = self
            .clean_state()
            .home_inputs
            .clone()
            .unwrap_or_else(|| (std::env::var_os("HOME").map(PathBuf::from), account_home()));
        trusted_home(env.as_deref(), account.as_deref())
            .map_err(|detail| FfiError::UntrustedHome { detail })
    }

    /// The saved-rules store of the trusted `home`. Tests must inject a temp
    /// one: falling back to the account's real storage would make them read
    /// and create files in the user's Application Support.
    pub(crate) fn protection_store(&self, home: &Path) -> ProtectionStore {
        match self.clean_state().protection_dir.clone() {
            Some(dir) => ProtectionStore::at(dir),
            None if cfg!(test) => panic!("a test must set `protection_dir` before executing"),
            None => ProtectionStore::for_home(home),
        }
    }
}

/// What the app may do with a category, and the tool it runs for it. The
/// Trash row and tool commands are offered only when the app asks for them.
/// A root-owned category (`needs_admin`) is report-only: macOS denies the
/// deletion even as root, because Full Disk Access is judged for
/// `authtrampoline`, not the app (`docs/internals/clean.md`).
fn desktop_action(
    providers: &[Box<dyn CleanProvider>],
    id: &str,
    options: &CleanOptions,
) -> (FfiDesktopAction, Option<DesktopTool>) {
    let provider = providers.iter().find(|p| p.id() == id);
    if options.include_trash && provider.is_some_and(|p| p.desktop_empties_trash()) {
        return (FfiDesktopAction::EmptyTrash, None);
    }
    let tool = provider.and_then(|p| p.desktop_tool());
    match tool.filter(|_| options.tool_commands) {
        Some(DesktopTool::Prune) => return (FfiDesktopAction::MoveToTrash, tool),
        Some(DesktopTool::Request) => return (FfiDesktopAction::RequestRemoval, tool),
        None => {}
    }
    let reason = match provider {
        Some(provider) => desktop_report_only(provider.as_ref()),
        None => Some(ReportOnly::NotPerPathTrash),
    };
    let action = match reason {
        None => FfiDesktopAction::MoveToTrash,
        Some(reason) => FfiDesktopAction::ReportOnly {
            reason: reason.into(),
        },
    };
    (action, None)
}

/// The provider's facts, or the path's own mtime when the category rule
/// alone selected it, so every candidate shows at least one fact.
fn candidate_evidence(item: &CleanItem) -> Vec<FfiEvidence> {
    if !item.evidence.is_empty() {
        return item.evidence.iter().cloned().map(Into::into).collect();
    }
    std::fs::symlink_metadata(&item.path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|since| FfiEvidence::Modified {
            at: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        })
        .into_iter()
        .collect()
}

fn store_category(
    discovery_id: u64,
    category: CheckedCategory,
    desktop_action: FfiDesktopAction,
    tool: Option<DesktopTool>,
    needs_admin: bool,
    candidates: &mut HashMap<String, StoredCandidate>,
) -> FfiCleanCategory {
    let family = known_category_ids()
        .contains(&category.id.as_str())
        .then(|| category_family(&category.id).id().to_string());
    let comes_back = comes_back(&category.id).map(Into::into);
    let section = category_section(&category.id).map(Into::into);
    let mut ffi = FfiCleanCategory {
        id: category.id,
        label: category.label,
        inclusion_reason: category.inclusion_reason,
        family,
        risk: category.risk.into(),
        status: FfiCategoryStatus::Found,
        desktop_action,
        candidates: Vec::new(),
        total_bytes: 0,
        unreadable: Vec::new(),
        refused: Vec::new(),
        comes_back,
        section,
        needs_admin,
    };
    match category.outcome {
        CategoryOutcome::Found {
            items,
            unreadable,
            roots,
            refused,
        } => {
            ffi.refused = refused
                .into_iter()
                .map(|(path, reason)| FfiRefusedPath {
                    path: path.to_string_lossy().into_owned(),
                    reason,
                })
                .collect();
            let roots = Arc::new(roots);
            for checked in items {
                let id = format!("d{discovery_id}-{}", candidates.len());
                let item = checked.item;
                ffi.total_bytes = ffi.total_bytes.saturating_add(item.size);
                ffi.candidates.push(FfiCleanCandidate {
                    id: id.clone(),
                    path: item.path.to_string_lossy().into_owned(),
                    size_bytes: item.size,
                    unreadable_entries: checked.unreadable.map_or(0, |u| u.entries),
                    risk: item.risk.into(),
                    evidence: candidate_evidence(&item),
                    size_is_upper_bound: checked.size_is_upper_bound,
                });
                // A tool's placeholder names no file: never stat it.
                let meta = item
                    .path
                    .is_absolute()
                    .then(|| std::fs::symlink_metadata(&item.path).ok())
                    .flatten();
                let fingerprint = match desktop_action {
                    FfiDesktopAction::EmptyTrash => PathFingerprint::of_listing,
                    _ => PathFingerprint::of,
                };
                candidates.insert(
                    id,
                    StoredCandidate {
                        fingerprint: meta.as_ref().map(fingerprint),
                        keep_scope: candidate_scope(desktop_action, &item.path, meta.as_ref()),
                        item,
                        roots: roots.clone(),
                        desktop_action,
                        size_is_upper_bound: checked.size_is_upper_bound,
                        shared: checked.shared,
                        tool,
                    },
                );
            }
            ffi.unreadable = unreadable.into_iter().map(unreadable_path).collect();
        }
        CategoryOutcome::AppRunning { app } => ffi.status = FfiCategoryStatus::AppRunning { app },
        CategoryOutcome::Unavailable { reason } => {
            ffi.status = FfiCategoryStatus::Unavailable { reason }
        }
        CategoryOutcome::Failed { error } => {
            ffi.status = FfiCategoryStatus::Failed { detail: error }
        }
    }
    ffi
}

/// Drops shared blocks no selection could complete, so the session keeps
/// only what can change a selection's total. Report-only candidates are
/// never selected, so theirs go entirely.
fn keep_completable_shared_blocks(candidates: &mut HashMap<String, StoredCandidate>) {
    let mut movable = Vec::new();
    for candidate in candidates.values_mut() {
        if matches!(
            candidate.desktop_action,
            FfiDesktopAction::ReportOnly { .. }
        ) {
            candidate.shared = SharedBlocks::default();
        } else {
            movable.push(&mut candidate.shared);
        }
    }
    SharedBlocks::retain_completable(&mut movable);
}

/// Every other candidate of the discovery at `path` or inside it, selected
/// or not. Moving `path` moves them too, so their providers' guards and app
/// gates must hold (e.g. streaming-caches' Spotify gate on a user-caches
/// folder). Sorted so the checks run in a stable order.
fn covered_candidates(discovery: &StoredDiscovery, id: &str, path: &Path) -> Vec<CleanItem> {
    let mut covered: Vec<CleanItem> = discovery
        .candidates
        .iter()
        .filter(|(other, candidate)| *other != id && candidate.item.path.starts_with(path))
        .map(|(_, candidate)| candidate.item.clone())
        .collect();
    covered.sort_by(|a, b| (&a.path, &a.category_id).cmp(&(&b.path, &b.category_id)));
    covered
}

/// Review-risk candidates at `path` or inside it that the user did not
/// select, sorted. Moving `path` would move them without explicit consent.
fn unselected_review(
    discovery: &StoredDiscovery,
    id: &str,
    path: &Path,
    selected: &BTreeSet<String>,
) -> Vec<String> {
    let mut ids: Vec<String> = discovery
        .candidates
        .iter()
        .filter(|(other, candidate)| {
            *other != id
                && candidate.item.risk != RiskLevel::Safe
                && candidate.item.path.starts_with(path)
                && !selected.contains(*other)
        })
        .map(|(other, _)| other.clone())
        .collect();
    ids.sort();
    ids
}

/// Resolves selected IDs against the stored discovery only.
fn plan_selection(
    discovery: &StoredDiscovery,
    candidate_ids: Vec<String>,
) -> Result<Vec<(String, PlannedItem)>, FfiError> {
    let unique: BTreeSet<String> = candidate_ids.into_iter().collect();
    if unique.is_empty() {
        return Err(FfiError::InvalidInput {
            detail: "empty selection".into(),
        });
    }
    unique
        .into_iter()
        .map(|id| {
            let candidate =
                discovery
                    .candidates
                    .get(&id)
                    .ok_or_else(|| FfiError::PreviewInvalid {
                        detail: format!("unknown candidate {id}; scan again"),
                    })?;
            let path = candidate.item.path.display();
            if matches!(
                candidate.desktop_action,
                FfiDesktopAction::ReportOnly { .. }
            ) {
                return Err(FfiError::InvalidInput {
                    detail: format!("{path} is report-only on the desktop"),
                });
            }
            // Only a tool's placeholder may have no fingerprint.
            let fingerprint = candidate.fingerprint.clone();
            if fingerprint.is_none() && candidate.desktop_action != FfiDesktopAction::RequestRemoval
            {
                return Err(FfiError::InvalidInput {
                    detail: format!("{path} could not be inspected at discovery"),
                });
            }
            let plan = PlannedItem {
                item: candidate.item.clone(),
                fingerprint,
                roots: candidate.roots.as_ref().clone(),
                covers: Vec::new(),
                // Decided once overlaps are merged.
                removal: Removal::MoveToTrash,
                size_is_upper_bound: candidate.size_is_upper_bound,
                shared: candidate.shared.clone(),
            };
            Ok((id, plan))
        })
        .collect()
}

fn exclusion(
    kept: &[(String, PlannedItem)],
    id: String,
    plan: &PlannedItem,
    overlap: Overlap,
) -> FfiPreviewExclusion {
    let reason = match overlap {
        Overlap::Duplicate { kept: index } => FfiExclusionReason::Duplicate {
            kept_candidate_id: kept[index].0.clone(),
        },
        Overlap::Inside { kept: index } => FfiExclusionReason::InsideSelected {
            parent_candidate_id: kept[index].0.clone(),
        },
    };
    FfiPreviewExclusion {
        candidate_id: id,
        path: plan.item.path.to_string_lossy().into_owned(),
        reason,
    }
}

fn preview_item(
    discovery: &StoredDiscovery,
    selected: &BTreeSet<String>,
    id: &str,
    plan: &PlannedItem,
) -> FfiPreviewItem {
    let mut covers: Vec<FfiCoveredCandidate> = discovery
        .candidates
        .iter()
        .filter(|(other, candidate)| {
            *other != id && candidate.item.path.starts_with(&plan.item.path)
        })
        .map(|(other, candidate)| FfiCoveredCandidate {
            candidate_id: other.clone(),
            path: candidate.item.path.to_string_lossy().into_owned(),
            risk: candidate.item.risk.into(),
            selected: selected.contains(other),
        })
        .collect();
    covers.sort_by(|a, b| (&a.path, &a.candidate_id).cmp(&(&b.path, &b.candidate_id)));
    FfiPreviewItem {
        candidate_id: id.to_string(),
        category_id: plan.item.category_id.clone(),
        path: plan.item.path.to_string_lossy().into_owned(),
        size_bytes: plan.item.size,
        risk: plan.item.risk.into(),
        covers,
        permanent: plan.removal != Removal::MoveToTrash,
        needs_admin: plan.removal == Removal::DeleteAsAdministrator,
    }
}

fn total_size<'a>(items: impl Iterator<Item = &'a CleanItem>) -> u64 {
    items.fold(0, |sum, item| sum.saturating_add(item.size))
}

fn unreadable_path((path, unreadable): (PathBuf, Unreadable)) -> FfiUnreadablePath {
    FfiUnreadablePath {
        path: path.to_string_lossy().into_owned(),
        entries: unreadable.entries,
        detail: unreadable.first_error,
    }
}
