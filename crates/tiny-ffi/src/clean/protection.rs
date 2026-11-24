//! Desktop "Always keep": what the saved rules say about the discovered
//! candidates, the preview exclusions they cause, and the only three writes
//! (keep a discovered candidate, keep a path the user picked, remove a saved
//! rule by ID). Rust owns the rules and their storage. A picked path is the
//! one thing Swift sends, and it is only ever kept, never moved: Rust checks
//! it, reads its kind from disk and refuses a folder that holds the home.
//!
//! Locking: session gate, then the store lock, then a short clean-state lock.
//! Reads (`clean_protection`, the preview) use the shared store lock and never
//! create files; only a write or an execution takes the exclusive lease.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use tiny_core::clean::protection::{
    validate_rule_path, Evaluation, Matcher, PolicySnapshot, ProtectionRule, Relation, RuleScope,
    INTERNAL_ID_PREFIX,
};

/// Where macOS shows the data volume a second time: `/System/Volumes/Data/Users`
/// is `/Users`.
const DATA_VOLUME: &str = "/System/Volumes/Data";
use tiny_core::clean::protection_store::StoreError;
use tiny_core::clean::trash_plan::{protected_reason, PlannedItem};

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
pub(crate) fn guards(policy: &PolicySnapshot, evaluation: &Evaluation) -> Vec<FfiProtectionGuard> {
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
        // A tool's placeholder names no file; a rule on a path its tool
        // deletes or works in skips it at run time
        // (`CleanProvider::tool_touches`).
        let found = match plan.item.path.is_absolute() {
            true => guards(policy, &policy.evaluate(&matcher, &plan.item.path)),
            false => Vec::new(),
        };
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
pub(crate) fn protection_error(error: StoreError) -> FfiError {
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
        StoreError::Duplicate(_) => FfiError::InvalidInput {
            detail: "this path is already kept".into(),
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

/// What a keep resolved, re-checked against disk before the write.
struct KeepTarget {
    path: PathBuf,
    scope: RuleScope,
    /// A path the user picked, not a scan's candidate: its refusals name no
    /// path and no scan.
    picked: bool,
}

/// A rule write as the caller named it: an ID, or the path a user picked.
enum Change {
    Keep(String),
    KeepPath(String),
    Unkeep(String),
}

impl KeepTarget {
    /// The path must still be what the user saw: a folder swapped for a file
    /// (or the reverse) would silently change what the rule covers.
    fn revalidate(&self) -> Result<(), FfiError> {
        let invalid = |scanned: &str, picked: &str| FfiError::InvalidInput {
            detail: match self.picked {
                true => format!("it {picked}; choose it again"),
                false => format!("{} {scanned}; scan again", self.path.display()),
            },
        };
        match fs::symlink_metadata(&self.path) {
            Ok(meta) if scope_of(&meta) == Some(self.scope) => Ok(()),
            Ok(_) => Err(invalid(
                "changed kind since the scan",
                "changed while it was being saved",
            )),
            Err(e) => {
                let why = format!("cannot be inspected now ({e})");
                Err(invalid(&why, &why))
            }
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
        match store.load().and_then(|policy| {
            self.remember_policy(&policy)?;
            Ok(policy)
        }) {
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

    /// Saves a rule for a path the user picked, in a scan or not: that file,
    /// or that folder and everything inside, at its real location (a
    /// symlink in the folders above it is resolved). Refuses a path that is not
    /// absolute and normalized, is missing, is a symlink or a special file,
    /// or is the home folder or a folder holding it (`InvalidInput`, with
    /// words the app can show). Otherwise as `clean_keep`: the gate, the
    /// store lock, `expected_revision`, and the stored preview dropped.
    /// Selects and moves nothing.
    pub fn clean_keep_path(
        &self,
        path: String,
        expected_revision: u64,
    ) -> Result<FfiProtection, FfiError> {
        self.change_rules(expected_revision, Change::KeepPath(path))
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
    pub(crate) fn load_policy(&self) -> Result<PolicySnapshot, FfiError> {
        let home = self.clean_home()?;
        let policy = self
            .protection_store(&home)
            .load()
            .map_err(protection_error)?;
        self.remember_policy(&policy).map_err(protection_error)?;
        Ok(policy)
    }

    /// Unlike the last write revision, this history survives errors and rescans.
    pub(crate) fn remember_policy(&self, policy: &PolicySnapshot) -> Result<(), StoreError> {
        let mut state = self.clean_state();
        if state.stored_policy_seen && !policy.stored {
            return Err(StoreError::StoreLost);
        }
        state.stored_policy_seen |= policy.stored;
        Ok(())
    }

    /// The current discovery's ID and candidates in scan order. Short lock.
    fn discovered_candidates(&self) -> (Option<String>, Vec<CandidateView>) {
        let state = self.clean_state();
        let Some(discovery) = state.discovery.as_ref() else {
            return (None, Vec::new());
        };
        let views: Vec<CandidateView> = discovery
            .candidate_ids()
            .filter_map(|id| Some((id, discovery.candidates.get(id)?)))
            .map(|(id, candidate)| CandidateView {
                id: id.to_string(),
                path: candidate.item.path.clone(),
                keep_scope: candidate.keep_scope,
            })
            .collect();
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
        Ok(KeepTarget {
            path,
            scope,
            picked: false,
        })
    }

    /// What a rule for a picked path would cover, read from disk. The rule is
    /// saved where the item really is (the folders above it resolved, its own
    /// name as the volume spells it, the short spelling of the data volume),
    /// so the list shows one honest path and a second pick of the same item
    /// is seen as the same. The picked item itself is never followed: a link
    /// is refused. A rule on the home folder or above it, under either
    /// spelling of the data volume, would hold every candidate, so it is
    /// refused rather than saved. The refusals name no path: the app says
    /// which one was asked.
    fn picked_target(picked: &str, home: &Path) -> Result<KeepTarget, FfiError> {
        let invalid = |detail: String| FfiError::InvalidInput { detail };
        let unnamed = |e| invalid(format!("it cannot be kept: {e}"));
        let unread = |e: io::Error| match e.kind() {
            io::ErrorKind::NotFound => invalid("nothing is at this path".into()),
            _ => invalid(format!("it cannot be inspected now ({e})")),
        };
        let spelled = Path::new(picked);
        validate_rule_path(spelled).map_err(unnamed)?;
        let (Some(parent), Some(name)) = (spelled.parent(), spelled.file_name()) else {
            return Err(invalid("it cannot be kept".into()));
        };
        let found = fs::canonicalize(parent).map_err(unread)?.join(name);
        let meta = fs::symlink_metadata(&found).map_err(unread)?;
        let scope = scope_of(&meta).ok_or_else(|| {
            invalid("it is a link or a special file; choose the item itself".into())
        })?;
        // Not a link, so this only gives its name the spelling the volume holds.
        let path = short_spelling(fs::canonicalize(&found).map_err(unread)?);
        validate_rule_path(&path).map_err(unnamed)?;
        let long_home = Path::new(DATA_VOLUME).join(home.strip_prefix("/").unwrap_or(home));
        let holds_home = protected_reason(&path, Some(home), false)
            .or_else(|| protected_reason(&path, Some(&long_home), false));
        if let Some(reason) = holds_home {
            return Err(invalid(format!(
                "it is {reason}, which holds everything Clean looks at; choose something inside it"
            )));
        }
        Ok(KeepTarget {
            path,
            scope,
            picked: true,
        })
    }

    /// One rule write under the gate and the exclusive store lock. The rules
    /// must be exactly what the caller last loaded, in revision and content,
    /// so an edit by another session or process is never overwritten.
    fn change_rules(&self, expected: u64, change: Change) -> Result<FfiProtection, FfiError> {
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        // After the gate: no scan is replacing the discovery meanwhile.
        let target = match &change {
            Change::Keep(candidate_id) => Some(self.keep_target(candidate_id)?),
            Change::KeepPath(path) => Some(Self::picked_target(path, &home)?),
            Change::Unkeep(_) => None,
        };
        let store = self.protection_store(&home);
        let mut lease = store.lease().map_err(protection_error)?;
        let current = lease.snapshot();
        self.remember_policy(current).map_err(protection_error)?;
        let seen = self.clean_state().seen_policy.clone();
        if !seen.is_some_and(|seen| seen == *current && seen.revision == expected) {
            return Err(FfiError::ProtectionChanged);
        }
        let written = match (&change, &target) {
            (Change::Keep(_) | Change::KeepPath(_), Some(target)) => {
                target.revalidate()?;
                if already_kept(lease.snapshot(), &target.path) {
                    return Err(FfiError::InvalidInput {
                        detail: "this path is already kept".into(),
                    });
                }
                self.drop_preview();
                lease.add(expected, &target.path, target.scope).map(drop)
            }
            (Change::Unkeep(id), _) => {
                self.drop_preview();
                lease.remove(expected, id)
            }
            (Change::Keep(_) | Change::KeepPath(_), None) => {
                unreachable!("a keep always resolves its target")
            }
        };
        match written {
            Ok(()) => {
                let policy = lease.snapshot().clone();
                self.remember_policy(&policy).map_err(protection_error)?;
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

/// Whether a saved rule already names `path`, under any spelling: two rules
/// for one item would leave it kept after the user removed one.
fn already_kept(policy: &PolicySnapshot, path: &Path) -> bool {
    let matcher = Matcher::new();
    policy.rules.iter().any(|rule| {
        let found = matcher.evaluate([rule], path);
        found.blocks.iter().any(|b| b.relation == Relation::Equal)
    })
}

/// Resolving links leaves the long spelling of the data volume alone, so it
/// is shortened here when both spellings name one item.
fn short_spelling(path: PathBuf) -> PathBuf {
    let Ok(rest) = path.strip_prefix(DATA_VOLUME) else {
        return path;
    };
    let short = Path::new("/").join(rest);
    let item = |p: &Path| fs::symlink_metadata(p).ok().map(|m| (m.dev(), m.ino()));
    match (item(&path), item(&short)) {
        (Some(long), Some(same)) if long == same => short,
        _ => path,
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
