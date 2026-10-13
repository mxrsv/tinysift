//! Desktop "Always keep": what the saved rules say about the discovered
//! candidates, the preview exclusions they cause, and the only two writes
//! (keep a discovered candidate, remove a saved rule by ID). Rust owns the
//! rules and their storage; neither write takes a path from Swift.
//!
//! Locking: session gate, then the store lock, then a short clean-state lock.
//! Reads (`clean_protection`, the preview) use the shared store lock and never
//! create files; only a write or an execution takes the exclusive lease.

use std::fs;
use std::path::{Path, PathBuf};

use tiny_core::clean::protection::{
    validate_rule_path, Evaluation, Matcher, PolicySnapshot, ProtectionRule, Relation, RuleScope,
    INTERNAL_ID_PREFIX,
};
use tiny_core::clean::protection_store::StoreError;
use tiny_core::clean::trash_plan::PlannedItem;

use super::{FfiDesktopAction, FfiExclusionReason, FfiPreviewExclusion};
use crate::{FfiError, TinySession};

/// What a rule covers: exactly the path, or the path and everything inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProtectionScope {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiProtectionRule {
    /// Stable and never reused; the only handle `clean_unkeep` accepts.
    pub id: String,
    pub path: String,
    pub scope: FfiProtectionScope,
    /// Whether anything is at the path now. A rule for a missing path stays
    /// saved and applies again if the path returns.
    pub path_exists: bool,
}

/// Why a path may not move. Never a Review risk.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProtectionGuard {
    /// The path is a saved rule's path, or inside a kept folder.
    Kept { rule_id: String, rule_path: String },
    /// Moving it would carry a saved rule's path along, whether or not the
    /// scan found that path.
    HoldsKept { rule_id: String, rule_path: String },
    /// It is, or holds, Tiny's own saved settings.
    TinyStorage,
    /// Whether a saved rule covers it could not be decided; treated as kept.
    Uncertain { detail: String },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCandidateProtection {
    pub candidate_id: String,
    /// What "Always keep" would protect; `None` when this candidate cannot be
    /// kept (report-only category, symlink, special file, tool placeholder).
    pub keep_scope: Option<FfiProtectionScope>,
    /// Strongest first; empty when no rule holds the path.
    pub guards: Vec<FfiProtectionGuard>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiProtectionStatus {
    Available {
        /// 0 until the first rule is saved; pass it to `clean_keep`/`clean_unkeep`.
        revision: u64,
        rules: Vec<FfiProtectionRule>,
        /// One entry per candidate of the current discovery, in scan order.
        candidates: Vec<FfiCandidateProtection>,
    },
    /// The saved rules cannot be read. This is not "no rules": nothing may be
    /// previewed, kept or moved until a retry reads them again.
    Unavailable { detail: String },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiProtection {
    /// The discovery `candidates` describe; `None` before a scan.
    pub discovery_id: Option<String>,
    /// The storage folder, for recovery guidance.
    pub location: String,
    pub status: FfiProtectionStatus,
}

impl From<RuleScope> for FfiProtectionScope {
    fn from(scope: RuleScope) -> Self {
        match scope {
            RuleScope::File => Self::File,
            RuleScope::Directory => Self::Directory,
        }
    }
}

/// The scope "Always keep" gives a path of this kind: never a symlink, which
/// is judged by its own metadata, not its target (R2).
fn scope_of(meta: &fs::Metadata) -> Option<RuleScope> {
    let kind = meta.file_type();
    if kind.is_dir() {
        Some(RuleScope::Directory)
    } else if kind.is_file() {
        Some(RuleScope::File)
    } else {
        None
    }
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Why the rules hold the path `evaluation` describes: saved rules, then
/// Tiny's own storage, then doubts. Empty means nothing holds it.
fn guards(policy: &PolicySnapshot, evaluation: &Evaluation) -> Vec<FfiProtectionGuard> {
    let mut found = Vec::new();
    let mut storage = false;
    for block in &evaluation.blocks {
        match policy.guarded().find(|rule| rule.id == block.rule_id) {
            Some(rule) if !rule.id.starts_with(INTERNAL_ID_PREFIX) => {
                let (rule_id, rule_path) = (rule.id.clone(), lossy(&rule.path));
                found.push(match block.relation {
                    Relation::CarriesRule => FfiProtectionGuard::HoldsKept { rule_id, rule_path },
                    Relation::Equal | Relation::InsideRule => {
                        FfiProtectionGuard::Kept { rule_id, rule_path }
                    }
                });
            }
            _ => storage = true,
        }
    }
    found.extend(storage.then_some(FfiProtectionGuard::TinyStorage));
    found.extend(
        evaluation
            .uncertain
            .iter()
            .map(|u| FfiProtectionGuard::Uncertain {
                detail: u.doubt.to_string(),
            }),
    );
    found
}

/// Splits a selection into what the rules allow and typed exclusions for what
/// they hold. Runs before overlaps are merged, so a held parent leaves its
/// independently selected children in place.
pub(super) fn split_protected(
    policy: &PolicySnapshot,
    planned: Vec<(String, PlannedItem)>,
) -> (Vec<(String, PlannedItem)>, Vec<FfiPreviewExclusion>) {
    let matcher = Matcher::new();
    let mut allowed = Vec::new();
    let mut excluded = Vec::new();
    for (id, plan) in planned {
        let found = guards(policy, &policy.evaluate(&matcher, &plan.item.path));
        match found.into_iter().next() {
            None => allowed.push((id, plan)),
            Some(held_by) => excluded.push(FfiPreviewExclusion {
                candidate_id: id,
                path: lossy(&plan.item.path),
                reason: FfiExclusionReason::Protected { held_by },
            }),
        }
    }
    (allowed, excluded)
}

/// Contention is `Busy`, like any overlapping operation. Anything else means
/// the saved rules cannot be trusted, so nothing may be previewed or moved.
pub(super) fn protection_error(error: StoreError) -> FfiError {
    match error {
        StoreError::Busy => FfiError::Busy,
        other => FfiError::ProtectionUnavailable {
            detail: other.to_string(),
        },
    }
}

/// A failed rule write: stale or externally changed rules are `Changed`, a
/// failure before the commit is `NotSaved`, and an unknown commit is
/// `SaveUncertain` (reload, never repeat).
fn write_error(error: StoreError) -> FfiError {
    match error {
        StoreError::Stale { .. } | StoreError::Changed | StoreError::StoreLost => {
            FfiError::ProtectionChanged
        }
        StoreError::Duplicate(id) => FfiError::InvalidInput {
            detail: format!("this path is already kept by saved rule {id}"),
        },
        StoreError::UnknownRule(id) => FfiError::InvalidInput {
            detail: format!("there is no saved rule {id}"),
        },
        StoreError::NotSaved(e) => FfiError::ProtectionNotSaved {
            detail: e.to_string(),
        },
        StoreError::CommitUncertain(e) => FfiError::ProtectionSaveUncertain {
            detail: e.to_string(),
        },
        other => protection_error(other),
    }
}

/// A discovered candidate as the projection needs it.
struct CandidateView {
    id: String,
    path: PathBuf,
    keep_scope: Option<RuleScope>,
}

/// The candidate a keep resolved, re-checked against disk before the write.
struct KeepTarget {
    path: PathBuf,
    scope: RuleScope,
}

/// A rule write as the caller named it: only IDs, never a path.
enum Change {
    Keep(String),
    Unkeep(String),
}

impl KeepTarget {
    /// The path must still be what the user saw: a folder swapped for a file
    /// (or the reverse) would silently change what the rule covers.
    fn revalidate(&self) -> Result<(), FfiError> {
        let shown = self.path.display();
        let invalid = |why: String| FfiError::InvalidInput {
            detail: format!("{shown} {why}; scan again"),
        };
        match fs::symlink_metadata(&self.path) {
            Ok(meta) if scope_of(&meta) == Some(self.scope) => Ok(()),
            Ok(_) => Err(invalid("changed kind since the scan".into())),
            Err(e) => Err(invalid(format!("cannot be inspected now ({e})"))),
        }
    }
}

#[uniffi::export]
impl TinySession {
    /// The saved "always keep" rules with their revision, and what they say
    /// about each candidate of the current discovery. Read-only: it never
    /// creates the store. An unreadable store is `Unavailable`, not empty
    /// rules; contention is `Busy`. The result is what a later write must
    /// pass back as its revision.
    pub fn clean_protection(&self) -> Result<FfiProtection, FfiError> {
        let home = self.clean_home()?;
        let store = self.protection_store(&home);
        let location = lossy(store.dir());
        match store.load() {
            Ok(policy) => {
                let view = self.protection_view(&policy, location);
                self.clean_state().seen_policy = Some(policy);
                Ok(view)
            }
            Err(StoreError::Busy) => Err(FfiError::Busy),
            Err(error) => {
                self.clean_state().seen_policy = None;
                Ok(FfiProtection {
                    discovery_id: self.discovered_candidates().0,
                    location,
                    status: FfiProtectionStatus::Unavailable {
                        detail: error.to_string(),
                    },
                })
            }
        }
    }

    /// Saves a rule for a candidate of the current discovery: its exact path,
    /// plus everything inside when it is a folder (`keep_scope`). Takes the
    /// operation gate and the store lock, requires `expected_revision` to be
    /// what the last `clean_protection` returned, and drops the stored
    /// preview. Selects and moves nothing.
    pub fn clean_keep(
        &self,
        candidate_id: String,
        expected_revision: u64,
    ) -> Result<FfiProtection, FfiError> {
        self.change_rules(expected_revision, Change::Keep(candidate_id))
    }

    /// Removes a saved rule by ID, under the same conditions as `clean_keep`.
    /// Never selects the freed path.
    pub fn clean_unkeep(
        &self,
        rule_id: String,
        expected_revision: u64,
    ) -> Result<FfiProtection, FfiError> {
        self.change_rules(expected_revision, Change::Unkeep(rule_id))
    }
}

impl TinySession {
    /// Reads the saved rules for a preview. A store that cannot be read fails
    /// it: nothing is ever authorized against rules Tiny does not know.
    pub(super) fn load_policy(&self) -> Result<PolicySnapshot, FfiError> {
        let home = self.clean_home()?;
        self.protection_store(&home)
            .load()
            .map_err(protection_error)
    }

    /// The current discovery's ID and candidates in scan order. Short lock.
    fn discovered_candidates(&self) -> (Option<String>, Vec<CandidateView>) {
        let state = self.clean_state();
        let Some(discovery) = state.discovery.as_ref() else {
            return (None, Vec::new());
        };
        let mut views: Vec<CandidateView> = discovery
            .candidates
            .iter()
            .map(|(id, candidate)| CandidateView {
                id: id.clone(),
                path: candidate.item.path.clone(),
                keep_scope: candidate.keep_scope,
            })
            .collect();
        // IDs end in the scan-order index, so length then text is numeric order.
        views.sort_by(|a, b| (a.id.len(), &a.id).cmp(&(b.id.len(), &b.id)));
        (Some(format!("d{}", discovery.id)), views)
    }

    fn protection_view(&self, policy: &PolicySnapshot, location: String) -> FfiProtection {
        let (discovery_id, views) = self.discovered_candidates();
        let matcher = Matcher::new();
        let candidates = views
            .into_iter()
            .map(|view| FfiCandidateProtection {
                // A tool placeholder is no path a rule can name.
                guards: match view.path.is_absolute() {
                    true => guards(policy, &policy.evaluate(&matcher, &view.path)),
                    false => Vec::new(),
                },
                keep_scope: view.keep_scope.map(Into::into),
                candidate_id: view.id,
            })
            .collect();
        FfiProtection {
            discovery_id,
            location,
            status: FfiProtectionStatus::Available {
                revision: policy.revision,
                rules: policy.rules.iter().map(rule_record).collect(),
                candidates,
            },
        }
    }

    /// Resolves a candidate ID of the current discovery to what a rule would
    /// cover. Nothing but the stored candidate is trusted.
    fn keep_target(&self, candidate_id: &str) -> Result<KeepTarget, FfiError> {
        let state = self.clean_state();
        let candidate = state
            .discovery
            .as_ref()
            .and_then(|discovery| discovery.candidates.get(candidate_id))
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: format!("unknown candidate {candidate_id}; scan again"),
            })?;
        let path = candidate.item.path.clone();
        let scope = candidate.keep_scope.ok_or_else(|| FfiError::InvalidInput {
            detail: format!("{} cannot be kept", path.display()),
        })?;
        Ok(KeepTarget { path, scope })
    }

    /// One rule write under the gate and the exclusive store lock. The rules
    /// must be exactly what the caller last loaded, in revision and content,
    /// so an edit by another session or process is never overwritten.
    fn change_rules(&self, expected: u64, change: Change) -> Result<FfiProtection, FfiError> {
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        // After the gate: a scan in flight has no discovery to resolve yet.
        let target = match &change {
            Change::Keep(candidate_id) => Some(self.keep_target(candidate_id)?),
            Change::Unkeep(_) => None,
        };
        let store = self.protection_store(&home);
        let mut lease = store.lease().map_err(protection_error)?;
        let current = lease.snapshot();
        let seen = self.clean_state().seen_policy.clone();
        if !seen.is_some_and(|seen| seen == *current && seen.revision == expected) {
            return Err(FfiError::ProtectionChanged);
        }
        let written = match (&change, &target) {
            (Change::Keep(_), Some(target)) => {
                target.revalidate()?;
                self.drop_preview();
                lease.add(expected, &target.path, target.scope).map(drop)
            }
            (Change::Unkeep(id), _) => {
                self.drop_preview();
                lease.remove(expected, id)
            }
            (Change::Keep(_), None) => unreachable!("a keep always resolves its candidate"),
        };
        match written {
            Ok(()) => {
                let policy = lease.snapshot().clone();
                let view = self.protection_view(&policy, lossy(store.dir()));
                self.clean_state().seen_policy = Some(policy);
                Ok(view)
            }
            Err(error) => {
                // Whatever the cause, the caller reloads before the next write.
                self.clean_state().seen_policy = None;
                Err(write_error(error))
            }
        }
    }

    /// A stored preview was built for the old rules.
    fn drop_preview(&self) {
        self.clean_state().preview = None;
    }
}

fn rule_record(rule: &ProtectionRule) -> FfiProtectionRule {
    FfiProtectionRule {
        id: rule.id.clone(),
        path: lossy(&rule.path),
        scope: rule.scope.into(),
        path_exists: fs::symlink_metadata(&rule.path).is_ok(),
    }
}

/// What "Always keep" would protect for a candidate found at `path` (`meta`
/// is its `symlink_metadata`): `None` unless the desktop may move it and a
/// rule could name it.
pub(super) fn candidate_scope(
    action: FfiDesktopAction,
    path: &Path,
    meta: Option<&fs::Metadata>,
) -> Option<RuleScope> {
    let movable = action == FfiDesktopAction::MoveToTrash;
    (movable && validate_rule_path(path).is_ok())
        .then(|| meta.and_then(scope_of))
        .flatten()
}
