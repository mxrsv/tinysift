//! Large Files for the native app: a read-only scan, a trusted preview, a
//! checked move to the Trash and an Undo. The session keeps what the scan
//! found; Swift names file IDs, then a preview ID, never a path to act on.
//! Nothing here deletes for good (docs/internals/clean.md, "Large Files").
//!
//! The consent contract is Clean's: a preview is built from the current
//! scan's IDs only, expires, is spent before the first move (it never runs
//! twice, even after a panic), is refused when a rescan replaced the list or
//! the saved "always keep" rules changed, and the operation gate refuses a
//! second Large Files scan or any write meanwhile (another feature's scan
//! may run beside this one). Locking: session gate, then the
//! saved-rules lease, then a short state lock that never spans a Trash call
//! or callback.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use std::os::unix::fs::MetadataExt;
use tiny_core::clean::failure_cause::Failure;
use tiny_core::clean::finder_trash::Trash;
use tiny_core::clean::fs_safe::PathFingerprint;
use tiny_core::clean::large_files::app_version::{AppVersions, PlutilVersions};
use tiny_core::clean::large_files::last_used::{LastUsed, SpotlightLastUsed};
use tiny_core::clean::large_files::move_run::{
    move_to_trash, MoveContext, MoveOutcome, MoveReport, PlannedMove,
};
use tiny_core::clean::large_files::owned::scan_known_locations;
use tiny_core::clean::large_files::undo::{undo_moves, UndoItem, UndoOutcome};
use tiny_core::clean::large_files::{
    scan_elsewhere, scan_home, scan_installers_above, ElsewhereRoots, HomeWalk, InstallerRoots,
    LargeFile, Limits, DEFAULT_SIZE_FILTER_BYTES, INSTALLER_MIN_BYTES, SCAN_FLOOR_BYTES,
    SIZE_FILTER_BYTES,
};
use tiny_core::clean::native_trash::NativeTrash;
use tiny_core::clean::open_files::{OpenFiles, ProcessOpenFiles};
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::clean::protection::{Matcher, PolicySnapshot};

use crate::clean::{app_runner, protection_error, FfiItemOutcome, PREVIEW_TTL};
use crate::session::{CancellationToken, ScanSlot};
use crate::{FfiError, ProgressListener, TinySession};

mod records;
#[cfg(test)]
mod tests;

pub use records::*;
use records::{cause, file_record};

/// Session-held Large Files state.
#[derive(Default)]
pub(crate) struct LargeFilesState {
    next_id: u64,
    scan: Option<StoredScan>,
    preview: Option<StoredPreview>,
    /// What Undo takes: the latest move's items that are in the Trash.
    undo: Option<StoredMove>,
    /// Test override for `PREVIEW_TTL`.
    ttl: Option<Duration>,
}

impl LargeFilesState {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

struct StoredScan {
    id: u64,
    /// The folders the scan listed; a move must still find its file in one.
    roots: Vec<PathBuf>,
    files: HashMap<String, LargeFile>,
}

struct StoredPreview {
    id: String,
    scan_id: u64,
    /// `(file ID, plan)` in the order asked.
    items: Vec<(String, PlannedMove)>,
    expires_at: Instant,
    /// Both clocks: `Instant` ignores wall-clock changes, `SystemTime` keeps
    /// running while the Mac sleeps, which `Instant` does not.
    expires_at_wall: SystemTime,
    /// Kept after use so a replay reports "already used", not "unknown".
    consumed: bool,
    /// The saved rules this preview was authorized against.
    policy: PolicySnapshot,
}

struct StoredMove {
    id: String,
    scan_id: u64,
    items: Vec<MovedFile>,
}

struct MovedFile {
    file_id: String,
    file: LargeFile,
    trashed: PathBuf,
    fingerprint: PathFingerprint,
}

#[uniffi::export]
impl TinySession {
    /// Scans for Large Files: installers in Downloads, Desktop and Documents
    /// whose app is in Applications, then one metadata-only walk of the home
    /// for Copies, "Only copy on this Mac" and "Other large files" (500 MB,
    /// 90 days), then the items apps own (group 5). Read-only. Holds the Large Files scan slot of the operation gate, so
    /// it returns `Busy` while a write or another Large Files scan runs;
    /// other features' scans do not refuse it. Replaces the
    /// previous scan and any preview; a cancelled scan changes nothing.
    pub fn large_files_scan(
        &self,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiLargeFilesScan, FfiError> {
        let home = self.clean_home()?;
        let limits = Limits::default();
        self.scan_large_files_of_home(&home, None, limits, &token, progress.as_ref())
    }

    /// `large_files_scan` or `large_files_scan_in` with the size filter for
    /// groups 3 and 4 (LV-L5). The app scans once at the floor and filters
    /// its list, so a change of filter keeps the picks. A minimum under the
    /// floor or a `root` outside the home is `InvalidInput`, and nothing is
    /// scanned.
    pub fn large_files_scan_with(
        &self,
        options: FfiLargeFilesScanOptions,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiLargeFilesScan, FfiError> {
        let limits = Limits::with_size_filter(options.groups_3_4_min_bytes).ok_or_else(|| {
            FfiError::InvalidInput {
                detail: "groups 3 and 4 cannot list below the scan floor".into(),
            }
        })?;
        let home = self.clean_home()?;
        let root = match &options.root {
            Some(root) => Some(large_files_root(&home, Path::new(root))?),
            None => None,
        };
        self.scan_large_files_of_home(&home, root.as_deref(), limits, &token, progress.as_ref())
    }

    /// `large_files_scan` for one folder of the home (Storage's "Find Large
    /// Files Here"): the same scan, every rule and label unchanged (a copy
    /// is still found wherever its pair lives), listing only what lies
    /// inside `root`, and a later move reaches only those files. `root` must
    /// be an existing folder inside the home; anything else is
    /// `InvalidInput`, and nothing is scanned.
    pub fn large_files_scan_in(
        &self,
        root: String,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiLargeFilesScan, FfiError> {
        let home = self.clean_home()?;
        let root = large_files_root(&home, Path::new(&root))?;
        let limits = Limits::default();
        self.scan_large_files_of_home(&home, Some(&root), limits, &token, progress.as_ref())
    }

    /// Builds a preview from file IDs of the current scan. Unknown or stale
    /// IDs and an empty list are refused; a file a saved "always keep" rule
    /// holds is listed in `held_ids` and left out. Unreadable saved rules fail
    /// it (`ProtectionUnavailable`). Replaces any earlier preview. Every item
    /// moves to the Trash.
    pub fn large_files_preview(
        &self,
        file_ids: Vec<String>,
    ) -> Result<FfiLargeFilesPreview, FfiError> {
        self.preview_large_files(&file_ids)
    }

    /// Moves the preview's files to the Trash through `NSFileManager`
    /// (`NativeTrash`), never Finder, so no Automation permission is asked.
    /// The preview is spent before the first move. Every outcome after that
    /// is in the report; a cancellation or a change in the saved rules stops
    /// it. Each file is checked again right before it moves (still the file
    /// the scan saw, on the home volume, inside a scanned folder, not held by
    /// a rule); what fails stays where it was.
    pub fn large_files_move(
        &self,
        preview_id: String,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiLargeFilesMoveReport, FfiError> {
        self.move_large_files_holding(
            &preview_id,
            &NativeTrash::default(),
            &ProcessOpenFiles::new(),
            &token,
            progress.as_ref(),
        )
    }

    /// Puts the files of a move back where they were. Nothing is replaced:
    /// an item whose original path is taken, whose folder is gone or that is
    /// no longer in the Trash stays put and says why. What was restored goes
    /// back into the scan's list; what was not can be tried again.
    pub fn large_files_undo(&self, move_id: String) -> Result<FfiLargeFilesUndoReport, FfiError> {
        let _gate = self.begin()?;
        let (items, scan_id) = {
            let state = self.large_files_state();
            let moved = state
                .undo
                .as_ref()
                .filter(|m| m.id == move_id)
                .ok_or_else(|| FfiError::PreviewInvalid {
                    detail: "nothing to undo for that move".into(),
                })?;
            let items: Vec<(String, LargeFile, UndoItem)> = moved
                .items
                .iter()
                .map(|m| {
                    (
                        m.file_id.clone(),
                        m.file.clone(),
                        UndoItem {
                            original: m.file.path.clone(),
                            trashed: m.trashed.clone(),
                            fingerprint: m.fingerprint.clone(),
                        },
                    )
                })
                .collect();
            (items, moved.scan_id)
        };
        let undo_items: Vec<UndoItem> = items.iter().map(|(_, _, u)| u.clone()).collect();
        let outcomes = undo_moves(&undo_items);
        let mut state = self.large_files_state();
        let mut restored = 0u64;
        let mut results = Vec::with_capacity(items.len());
        for ((file_id, file, _), outcome) in items.into_iter().zip(outcomes) {
            if outcome == UndoOutcome::Restored {
                restored += 1;
                if let Some(scan) = state.scan.as_mut().filter(|s| s.id == scan_id) {
                    scan.files.insert(file_id.clone(), file.clone());
                }
                if let Some(moved) = state.undo.as_mut() {
                    moved.items.retain(|m| m.file_id != file_id);
                }
            }
            results.push(FfiLargeFileUndoResult {
                file_id,
                path: file.path.to_string_lossy().into_owned(),
                outcome: outcome.into(),
            });
        }
        if state.undo.as_ref().is_some_and(|m| m.items.is_empty()) {
            state.undo = None;
        }
        Ok(FfiLargeFilesUndoReport {
            results,
            restored_count: restored,
        })
    }
}

/// The Large Files size floor and filter choices (LV-L5): the numbers live
/// here, the app shows them.
#[uniffi::export]
pub fn large_files_size_filters() -> FfiLargeFilesSizeFilters {
    FfiLargeFilesSizeFilters {
        floor_bytes: SCAN_FLOOR_BYTES,
        choices_bytes: SIZE_FILTER_BYTES.to_vec(),
        default_bytes: DEFAULT_SIZE_FILTER_BYTES,
    }
}

/// The home walk of a scan (groups 3 and 4), injectable for tests.
pub(crate) struct HomeSettings<'a> {
    pub home: &'a std::path::Path,
    pub limits: Limits,
    pub last_used: &'a dyn LastUsed,
    /// Whether UTM or Cap is running (group 5).
    pub probe: &'a dyn AppProbe,
}

/// The folder a scan of part of the home lists, in the home's own spelling
/// (the walk's paths start with `home` as given): `root` resolved, an
/// existing folder, and `home` itself or inside it. Large Files never lists
/// anything outside the home (NL1), so another root is refused.
pub(crate) fn large_files_root(home: &Path, root: &Path) -> Result<PathBuf, FfiError> {
    let invalid = |detail: &str| FfiError::InvalidInput {
        detail: detail.into(),
    };
    if !root.is_absolute() {
        return Err(invalid("the folder must be an absolute path"));
    }
    let real = fs::canonicalize(root).map_err(|_| invalid("that folder could not be found"))?;
    if !fs::metadata(&real).is_ok_and(|meta| meta.is_dir()) {
        return Err(invalid("that is not a folder"));
    }
    let real_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let inside = real
        .strip_prefix(&real_home)
        .map_err(|_| invalid("Large Files looks only inside your home folder"))?;
    Ok(home.join(inside))
}

impl TinySession {
    fn large_files_state(&self) -> MutexGuard<'_, LargeFilesState> {
        self.large_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The whole scan of `home` with the app's own tools, listing only what
    /// lies inside `within` when it is set.
    fn scan_large_files_of_home(
        &self,
        home: &Path,
        within: Option<&Path>,
        limits: Limits,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiLargeFilesScan, FfiError> {
        let runner = app_runner();
        let versions = PlutilVersions(runner.as_ref());
        let last_used = SpotlightLastUsed(runner.as_ref());
        let probe = RunnerProbe(runner.as_ref());
        self.scan_large_files_within(
            &InstallerRoots::for_home(home),
            INSTALLER_MIN_BYTES,
            &versions,
            Some(HomeSettings {
                home,
                limits,
                last_used: &last_used,
                probe: &probe,
            }),
            within,
            token,
            progress,
        )
    }

    /// `large_files_scan` with injected roots, threshold and versions (the
    /// test seam); `walk` is `None` for a scan of installers only.
    #[cfg(test)]
    pub(crate) fn scan_large_files(
        &self,
        roots: &InstallerRoots,
        min_bytes: u64,
        versions: &dyn AppVersions,
        walk: Option<HomeSettings<'_>>,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiLargeFilesScan, FfiError> {
        self.scan_large_files_within(roots, min_bytes, versions, walk, None, token, progress)
    }

    /// `scan_large_files` listing only what lies inside `within` when it is
    /// set. The scan itself is the whole one, so what a file is called (a
    /// copy, the only copy) never depends on the folder asked for; the list
    /// and the folders a move may reach are narrowed to `within` afterwards.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scan_large_files_within(
        &self,
        roots: &InstallerRoots,
        min_bytes: u64,
        versions: &dyn AppVersions,
        walk: Option<HomeSettings<'_>>,
        within: Option<&Path>,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiLargeFilesScan, FfiError> {
        let _gate = self.begin_scan(ScanSlot::LargeFiles)?;
        progress.on_progress(progress_event("scan", 0, "Looking for installers"));
        token.check()?;
        let mut found = scan_installers_above(min_bytes, roots, versions, Some(token.flag()));
        token.check()?;
        let mut scanned_roots = roots.folders.clone();
        if let Some(walk) = walk {
            let claimed = found.files.iter().map(|f| f.path.clone()).collect();
            let on_progress = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
            let home = scan_home(
                &HomeWalk {
                    home: walk.home,
                    limits: walk.limits,
                    last_used: walk.last_used,
                    now: SystemTime::now(),
                    claimed: &claimed,
                },
                Some(token.flag()),
                Some(&on_progress),
            );
            token.check()?;
            // Group 5: app libraries, items apps own (met by the walk or in
            // the known locations), and every large file that frees nothing.
            let device = fs::metadata(walk.home).map_or(0, |meta| meta.dev());
            let known = scan_known_locations(
                walk.home,
                device,
                walk.limits.cloud_min_bytes,
                Some(token.flag()),
            );
            token.check()?;
            let mut frees_nothing = found.frees_nothing;
            frees_nothing.extend(home.frees_nothing);
            let mut owned = home.owned;
            owned.extend(known.items);
            let elsewhere = scan_elsewhere(
                &ElsewhereRoots {
                    home: walk.home,
                    applications: &roots.applications,
                    probe: walk.probe,
                    owned_min_bytes: walk.limits.owned_min_bytes,
                },
                frees_nothing,
                owned,
                Some(token.flag()),
            );
            token.check()?;
            found.files.extend(home.files);
            found.files.extend(elsewhere);
            found.unreadable.extend(home.unreadable);
            found.unreadable.extend(known.unreadable);
            // A move may reach any file the walk listed, still through real folders.
            scanned_roots.push(walk.home.to_path_buf());
        }
        if let Some(root) = within {
            found.files.retain(|file| file.path.starts_with(root));
            // A folder above `root` that could not be read hid part of it too.
            found
                .unreadable
                .retain(|folder| folder.starts_with(root) || root.starts_with(folder));
            scanned_roots = vec![root.to_path_buf()];
        }
        let mut state = self.large_files_state();
        let id = state.next_id();
        let mut files = HashMap::new();
        let mut records = Vec::with_capacity(found.files.len());
        for (index, file) in found.files.into_iter().enumerate() {
            let file_id = format!("l{id}-{index}");
            records.push(file_record(&file_id, &file));
            files.insert(file_id, file);
        }
        state.scan = Some(StoredScan {
            id,
            roots: scanned_roots,
            files,
        });
        state.preview = None;
        progress.on_progress(progress_event("scan", 1, "Done"));
        Ok(FfiLargeFilesScan {
            scan_id: format!("l{id}"),
            files: records,
            unreadable_folders: found
                .unreadable
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        })
    }

    pub(crate) fn preview_large_files(
        &self,
        file_ids: &[String],
    ) -> Result<FfiLargeFilesPreview, FfiError> {
        let invalid = |detail: &str| FfiError::PreviewInvalid {
            detail: detail.into(),
        };
        if file_ids.is_empty() {
            return Err(FfiError::InvalidInput {
                detail: "nothing is picked".into(),
            });
        }
        // Rules are read without the state lock (lock order).
        let policy = self.load_policy()?;
        let matcher = Matcher::new();
        let mut state = self.large_files_state();
        let state = &mut *state;
        let scan = state
            .scan
            .as_ref()
            .ok_or_else(|| invalid("no current scan; scan again"))?;
        let mut seen = std::collections::HashSet::new();
        let (mut items, mut held, mut plan) = (Vec::new(), Vec::new(), Vec::new());
        for id in file_ids.iter().filter(|id| seen.insert(id.as_str())) {
            let file = scan
                .files
                .get(id)
                .ok_or_else(|| invalid("unknown file; scan again"))?;
            // Group 5 is listed, never moved from here (OW1, FN1).
            if !file.group.movable() {
                return Err(FfiError::InvalidInput {
                    detail: "that item is managed elsewhere and cannot be moved from here".into(),
                });
            }
            // The file that stays must not go in the same move (CP1), nor
            // anything of an archive's unpacked copy picked with it (LV-L6).
            if file_ids.iter().any(|other| {
                scan.files.get(other).is_some_and(|f| {
                    f.copy.as_ref().is_some_and(|c| c.keeper == file.path)
                        || f.extracted
                            .as_ref()
                            .is_some_and(|e| file.path.starts_with(&e.beside))
                })
            }) {
                return Err(FfiError::InvalidInput {
                    detail: "that file is the one that stays for a copy picked with it".into(),
                });
            }
            if !policy.evaluate(&matcher, &file.path).allows_move() {
                held.push(id.clone());
                continue;
            }
            items.push(file_record(id, file));
            plan.push((
                id.clone(),
                PlannedMove {
                    path: file.path.clone(),
                    fingerprint: file.fingerprint.clone(),
                    // A copy goes only while what stays is still there: the
                    // original, or the largest file of the unpacked archive.
                    must_remain: file
                        .copy
                        .as_ref()
                        .map(|c| (c.keeper.clone(), c.keeper_fingerprint.clone()))
                        .or_else(|| {
                            file.extracted
                                .as_ref()
                                .map(|e| (e.anchor.clone(), e.anchor_fingerprint.clone()))
                        }),
                    // ...and an archive only while all of its unpacked copy is.
                    must_stay_unpacked: file.extracted.as_ref().map(|e| e.beside.clone()),
                },
            ));
        }
        let scan_id = scan.id;
        let ttl = state.ttl.unwrap_or(PREVIEW_TTL);
        let preview_id = format!("lp{}", state.next_id());
        let expires_at_wall = SystemTime::now() + ttl;
        let frees_bytes = items
            .iter()
            .fold(0u64, |s, i| s.saturating_add(i.frees_bytes));
        let frees_is_exact = items.iter().all(|i| i.frees_is_exact);
        state.preview = Some(StoredPreview {
            id: preview_id.clone(),
            scan_id,
            items: plan,
            expires_at: Instant::now() + ttl,
            expires_at_wall,
            consumed: false,
            policy,
        });
        Ok(FfiLargeFilesPreview {
            preview_id,
            items,
            held_ids: held,
            frees_bytes,
            frees_is_exact,
            expires_in_seconds: ttl.as_secs(),
        })
    }

    /// `large_files_move` with an injected Trash (the test seam). The saved
    /// rules stay locked until the report is built, and the lease is taken
    /// before the preview is spent, so a busy or unreadable store fails
    /// without spending it.
    #[cfg(test)]
    pub(crate) fn move_large_files(
        &self,
        preview_id: &str,
        trash: &dyn Trash,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiLargeFilesMoveReport, FfiError> {
        let nothing_held = tiny_core::clean::open_files::NoOpenFiles;
        self.move_large_files_holding(preview_id, trash, &nothing_held, token, progress)
    }

    /// `move_large_files` while running processes hold what `open_files`
    /// says; `move_large_files` itself runs as if nothing were held.
    pub(crate) fn move_large_files_holding(
        &self,
        preview_id: &str,
        trash: &dyn Trash,
        open_files: &dyn OpenFiles,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiLargeFilesMoveReport, FfiError> {
        let home = self.clean_home()?;
        let _gate = self.begin()?;
        let store = self.protection_store(&home);
        let policy = store.lease().map_err(protection_error)?;
        self.remember_policy(policy.snapshot())
            .map_err(protection_error)?;
        let (items, scan_id, roots, files) =
            self.consume_large_files_preview(preview_id, policy.snapshot())?;
        let plan: Vec<PlannedMove> = items.iter().map(|(_, p)| p.clone()).collect();
        let on_progress = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let ctx = MoveContext {
            home: &home,
            roots: &roots,
            trash,
            open_files,
            cancel: Some(token.flag()),
            progress: Some(&on_progress),
            policy: &policy,
        };
        let executed = move_to_trash(&plan, &ctx);
        Ok(self.record_move(scan_id, &items, &files, executed))
    }

    /// Marks the preview consumed, under the lock, before any mutation.
    #[allow(clippy::type_complexity)]
    fn consume_large_files_preview(
        &self,
        preview_id: &str,
        policy: &PolicySnapshot,
    ) -> Result<
        (
            Vec<(String, PlannedMove)>,
            u64,
            Vec<PathBuf>,
            HashMap<String, LargeFile>,
        ),
        FfiError,
    > {
        let invalid = |detail: &str| FfiError::PreviewInvalid {
            detail: detail.into(),
        };
        let mut state = self.large_files_state();
        let state = &mut *state;
        let scan = state.scan.as_ref();
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
        let scan = scan
            .filter(|s| s.id == preview.scan_id)
            .ok_or_else(|| invalid("the list was replaced by a new scan; scan again"))?;
        if preview.policy != *policy {
            state.preview = None;
            return Err(FfiError::ProtectionChanged);
        }
        preview.consumed = true;
        let files = preview
            .items
            .iter()
            .filter_map(|(id, _)| scan.files.get(id).map(|f| (id.clone(), f.clone())))
            .collect();
        Ok((
            preview.items.clone(),
            preview.scan_id,
            scan.roots.clone(),
            files,
        ))
    }

    /// The report, with what moved dropped from the scan's list and kept for
    /// Undo (replacing an earlier move's).
    fn record_move(
        &self,
        scan_id: u64,
        items: &[(String, PlannedMove)],
        files: &HashMap<String, LargeFile>,
        executed: MoveReport,
    ) -> FfiLargeFilesMoveReport {
        let mut moved = Vec::new();
        let mut results = Vec::with_capacity(items.len());
        let (mut failed, mut skipped, mut not_attempted) = (0u64, 0u64, 0u64);
        for ((id, _), result) in items.iter().zip(&executed.results) {
            let trashed_path = match &result.outcome {
                MoveOutcome::Moved { trashed_to } => trashed_to.clone(),
                _ => None,
            };
            if let (Some(file), Some(trashed), Some(fingerprint)) =
                (files.get(id), &trashed_path, &result.fingerprint)
            {
                moved.push(MovedFile {
                    file_id: id.clone(),
                    file: file.clone(),
                    trashed: trashed.clone(),
                    fingerprint: fingerprint.clone(),
                });
            }
            match &result.outcome {
                MoveOutcome::Failed { .. } => failed += 1,
                MoveOutcome::Skipped(_) => skipped += 1,
                MoveOutcome::NotAttempted => not_attempted += 1,
                MoveOutcome::Moved { .. } => {}
            }
            results.push(FfiLargeFileMoveResult {
                file_id: id.clone(),
                path: result.path.to_string_lossy().into_owned(),
                outcome: ffi_outcome(&result.outcome),
                cause: cause(result.failure.as_ref()),
                raw_error: result
                    .failure
                    .as_ref()
                    .map(|f: &Failure| f.raw_error.clone()),
                bytes_moved: result.bytes_moved,
                bytes_moved_is_upper_bound: result.bytes_moved_is_upper_bound,
                trashed_path: trashed_path.map(|p| p.to_string_lossy().into_owned()),
            });
        }
        let mut state = self.large_files_state();
        // Whatever moved is gone from the list, whether Undo can find it or not.
        if let Some(scan) = state.scan.as_mut().filter(|s| s.id == scan_id) {
            for ((id, _), result) in items.iter().zip(&executed.results) {
                if matches!(result.outcome, MoveOutcome::Moved { .. }) {
                    scan.files.remove(id);
                }
            }
        }
        let move_id = (!moved.is_empty()).then(|| format!("lm{}", state.next_id()));
        // A run that moved nothing leaves the previous move's Undo as it was.
        if let Some(id) = &move_id {
            state.undo = Some(StoredMove {
                id: id.clone(),
                scan_id,
                items: moved,
            });
        }
        FfiLargeFilesMoveReport {
            move_id,
            bytes_moved_to_trash: executed.bytes_moved(),
            bytes_moved_is_upper_bound: executed
                .results
                .iter()
                .any(|r| r.bytes_moved > 0 && r.bytes_moved_is_upper_bound),
            moved_count: executed.moved_count() as u64,
            failed_count: failed,
            skipped_count: skipped,
            not_attempted_count: not_attempted,
            stopped: executed.stopped.map(Into::into),
            results,
        }
    }
}

fn ffi_outcome(outcome: &MoveOutcome) -> FfiItemOutcome {
    use tiny_core::clean::checked_execute::ItemOutcome;
    match outcome.clone() {
        MoveOutcome::Moved { .. } => ItemOutcome::MovedToTrash,
        MoveOutcome::Failed {
            error,
            source_present,
        } => ItemOutcome::Failed {
            error,
            source_present,
        },
        MoveOutcome::Skipped(reason) => ItemOutcome::Skipped(reason),
        MoveOutcome::NotAttempted => ItemOutcome::NotAttempted,
    }
    .into()
}

fn progress_event(operation: &str, completed: u64, message: &str) -> crate::FfiProgress {
    crate::FfiProgress {
        operation: operation.into(),
        completed,
        total: Some(1),
        message: message.into(),
        found_bytes: None,
        category: None,
        current_category: None,
    }
}
