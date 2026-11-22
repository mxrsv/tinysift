//! The Browsers page's write side: a profile's Privacy clean and its Undo,
//! over core's `remove_privacy` and `undo_privacy`.
//!
//! The clean takes as consent exactly the cookie keys and history rows the
//! named scan stored for the profile (`BrowsersState::consented`); Swift
//! names a generation and a profile, never an item. Both calls hold the
//! write gate, so neither overlaps a Browsers scan or another write. Both
//! run with the real running check (Clean's `pgrep` names plus every live
//! process's outer app bundle), the native Trash and the account's Undo
//! store, `~/Library/Application Support/Tiny/browser-privacy/`.
//!
//! A clean that ran, or whose edit failed partway, spends the consent; one
//! refused before anything changed (the browser runs, a file changed, the
//! backup failed) leaves it usable, so quitting the browser and pressing
//! Clean again needs no new scan. Undo lasts until the profile's next scan:
//! the app must not rescan while it offers Undo.

use std::path::Path;
use std::time::SystemTime;

use tiny_core::browsers::privacy::{
    remove_privacy, undo_privacy, BrowserProcesses, LiveBundles, PrivacyConsent, PrivacyEnv,
    PrivacyIssue, PrivacyIssueKind, PrivacyUndoError, PrivacyUndoReport, PrivacyWriteError,
    PrivacyWriteReport, RunningCheck, SkipReason, UndoStore,
};
use tiny_core::browsers::BrowserKind;
use tiny_core::clean::finder_trash::Trash;
use tiny_core::clean::native_trash::NativeTrash;
use tiny_core::clean::process::RunnerProbe;

use super::records::{FfiBrowserIssue, FfiBrowserKind};
use crate::clean::app_runner;
use crate::{FfiError, TinySession};

/// What a Privacy clean removed from one profile.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiBrowserPrivacyReport {
    pub profile_id: String,
    /// Cookie rows removed (one stored key may cover several rows).
    pub removed_cookies: u64,
    /// History entries removed: Chromium `urls` or Firefox `moz_places`
    /// rows, each with its visits.
    pub removed_history_entries: u64,
    /// Favicon mappings, top sites and shortcuts removed with them.
    pub removed_leftovers: u64,
    /// Consented cookie keys left alone.
    pub skipped_cookies: FfiPrivacySkips,
    /// Consented history entries left alone.
    pub skipped_history: FfiPrivacySkips,
    /// The backup folder in the Trash; `None` when nothing was removed.
    pub backup_path: Option<String>,
    /// For `browsers_undo_privacy`; `None` when nothing changed or the Undo
    /// record could not be kept (see `issues`).
    pub undo_id: Option<String>,
    /// Diagnostic text for what did not complete after the edit itself
    /// succeeded (a file Chromium rebuilds that stayed, a lost Undo).
    pub issues: Vec<String>,
}

/// Why consented items were left alone, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct FfiPrivacySkips {
    /// Kept now: the site was visited or the cookie used since the scan,
    /// or a bookmark points at the history entry.
    pub now_kept: u64,
    /// The profile no longer holds it.
    pub not_found: u64,
}

/// What an Undo put back.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiBrowserUndoReport {
    pub profile_id: String,
    /// Files whose original bytes are back in place.
    pub restored_files: u64,
    /// Files the clean had created, now in the Trash.
    pub trashed_files: u64,
}

/// Why a Privacy clean or its Undo changed nothing. User-facing copy lives
/// in Swift; `detail` is diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiBrowserPrivacyRefusal {
    /// A process of the browser (the app or a helper) runs. The page shows
    /// the quit line; TinySift never quits it.
    Running { kind: FfiBrowserKind },
    /// Whether the browser runs could not be told, so nothing was touched.
    RunningUnknown {
        kind: FfiBrowserKind,
        detail: String,
    },
    /// The browser's privacy data is not edited in this build (Safari), or
    /// a database could not be read or has a schema the edit does not know.
    Unsupported { issue: FfiBrowserIssue },
    /// Clean: a browser file changed or moved while the clean ran. Try
    /// again.
    Changed { detail: String },
    /// The backup could not be made or checked (clean), or is missing or
    /// changed in the Trash (Undo).
    Backup { detail: String },
    /// The Undo store could not be read, locked or written.
    UndoStore { detail: String },
    /// Undo: nothing to undo under that id (already undone, or a later
    /// clean of the profile replaced it).
    NothingToUndo,
    /// Undo: the profile was scanned since that clean (B25).
    Expired,
    /// Undo: that clean did not finish; its backup is in the Trash.
    Unfinished,
    /// Undo: a file changed since the clean (the browser ran), so nothing
    /// was put back.
    ChangedSinceClean { path: String },
}

#[uniffi::export]
impl TinySession {
    /// Removes the profile's old and tracker cookies and its history older
    /// than 90 days, exactly as the Browsers scan `generation` counted them,
    /// after a backup of every touched file goes to the Trash. Refused
    /// (`BrowserPrivacyRefused`) while any process of the browser runs.
    /// `PreviewInvalid` when a newer scan replaced `generation` or the
    /// profile was already cleaned under it; `Busy` while a scan or another
    /// write runs. A failed edit is `BrowserPrivacyFailed`, with an Undo id
    /// when some file did change.
    pub fn browsers_clean_privacy(
        &self,
        generation: u64,
        kind: FfiBrowserKind,
        profile_id: String,
    ) -> Result<FfiBrowserPrivacyReport, FfiError> {
        let home = self.clean_home()?;
        let runner = app_runner();
        let names = RunnerProbe(runner.as_ref());
        let bundles = LiveBundles {
            runner: runner.as_ref(),
        };
        let processes = RunningCheck {
            names: &names,
            bundles: &bundles,
        };
        self.clean_browser_privacy(
            &home,
            generation,
            kind.into(),
            &profile_id,
            &processes,
            &NativeTrash::default(),
            SystemTime::now(),
        )
    }

    /// Puts back, byte for byte, the files the clean `undo_id` of the
    /// profile changed. Applies until the profile's next Browsers scan and
    /// only while nothing changed them since; refused while the browser
    /// runs. The backup stays in the Trash.
    pub fn browsers_undo_privacy(
        &self,
        kind: FfiBrowserKind,
        profile_id: String,
        undo_id: String,
    ) -> Result<FfiBrowserUndoReport, FfiError> {
        let home = self.clean_home()?;
        let runner = app_runner();
        let names = RunnerProbe(runner.as_ref());
        let bundles = LiveBundles {
            runner: runner.as_ref(),
        };
        let processes = RunningCheck {
            names: &names,
            bundles: &bundles,
        };
        self.undo_browser_privacy(
            &home,
            kind.into(),
            &profile_id,
            &undo_id,
            &processes,
            &NativeTrash::default(),
        )
    }
}

impl TinySession {
    /// `browsers_clean_privacy` with its home, running check, Trash and
    /// clock passed in (the test seam). The Undo store is `home`'s.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn clean_browser_privacy(
        &self,
        home: &Path,
        generation: u64,
        kind: BrowserKind,
        profile_id: &str,
        processes: &dyn BrowserProcesses,
        trash: &dyn Trash,
        now: SystemTime,
    ) -> Result<FfiBrowserPrivacyReport, FfiError> {
        let _gate = self.begin()?;
        let (profile, consent) = {
            let state = self.browsers_state();
            let stored = state.consented(generation, kind, profile_id)?;
            if !stored.privacy.supported {
                let issue = stored.privacy.reason.clone().unwrap_or(PrivacyIssue {
                    kind: PrivacyIssueKind::Unreadable,
                    path: None,
                    detail: "the profile's privacy data was not scanned".into(),
                });
                return Err(refused(FfiBrowserPrivacyRefusal::Unsupported {
                    issue: (&issue).into(),
                }));
            }
            let consent = PrivacyConsent {
                cookies: stored.privacy.cookies_to_remove.clone(),
                history: stored.privacy.history_to_remove.clone(),
            };
            (stored.profile.clone(), consent)
        };
        if consent.cookies.is_empty() && consent.history.is_empty() {
            return Ok(PrivacyWriteReport {
                profile_id: profile.id,
                ..PrivacyWriteReport::default()
            }
            .into());
        }
        let store = UndoStore::for_home(home);
        let env = PrivacyEnv {
            processes,
            trash,
            store: &store,
        };
        let outcome = remove_privacy(kind, &profile, &consent, &env, now);
        // Spent only when something may have changed: a refusal (also an
        // edit that failed before its commit) leaves the consent usable.
        if matches!(outcome, Ok(_) | Err(PrivacyWriteError::Write { .. })) {
            self.browsers_state().spend(generation, kind, profile_id);
        }
        outcome
            .map(Into::into)
            .map_err(|error| write_error(kind, error))
    }

    /// `browsers_undo_privacy` with its home, running check and Trash
    /// passed in (the test seam).
    pub(crate) fn undo_browser_privacy(
        &self,
        home: &Path,
        kind: BrowserKind,
        profile_id: &str,
        undo_id: &str,
        processes: &dyn BrowserProcesses,
        trash: &dyn Trash,
    ) -> Result<FfiBrowserUndoReport, FfiError> {
        let _gate = self.begin()?;
        let profile = self
            .browsers_state()
            .scanned(kind, profile_id)?
            .profile
            .clone();
        let store = UndoStore::for_home(home);
        let env = PrivacyEnv {
            processes,
            trash,
            store: &store,
        };
        undo_privacy(kind, &profile, undo_id, &env)
            .map(Into::into)
            .map_err(|error| undo_error(kind, error))
    }
}

fn refused(refusal: FfiBrowserPrivacyRefusal) -> FfiError {
    FfiError::BrowserPrivacyRefused { refusal }
}

pub(super) fn write_error(kind: BrowserKind, error: PrivacyWriteError) -> FfiError {
    use FfiBrowserPrivacyRefusal as R;
    let refusal = match error {
        PrivacyWriteError::NotSupported(browser) => R::Unsupported {
            issue: FfiBrowserIssue {
                kind: super::FfiBrowserIssueKind::NotSupported,
                path: None,
                detail: format!("{browser} privacy data is not edited in this build"),
            },
        },
        PrivacyWriteError::Unsupported(issue) => R::Unsupported {
            issue: (&issue).into(),
        },
        PrivacyWriteError::Running(_) => R::Running { kind: kind.into() },
        PrivacyWriteError::RunningUnknown { detail, .. } => R::RunningUnknown {
            kind: kind.into(),
            detail,
        },
        PrivacyWriteError::Backup(detail) => R::Backup { detail },
        PrivacyWriteError::Store(detail) => R::UndoStore { detail },
        PrivacyWriteError::Changed(detail) => R::Changed { detail },
        PrivacyWriteError::Write { detail, undo_id } => {
            return FfiError::BrowserPrivacyFailed { detail, undo_id }
        }
    };
    refused(refusal)
}

pub(super) fn undo_error(kind: BrowserKind, error: PrivacyUndoError) -> FfiError {
    use FfiBrowserPrivacyRefusal as R;
    let refusal = match error {
        PrivacyUndoError::NoRecord => R::NothingToUndo,
        PrivacyUndoError::Expired => R::Expired,
        PrivacyUndoError::Unfinished => R::Unfinished,
        PrivacyUndoError::Running(_) => R::Running { kind: kind.into() },
        PrivacyUndoError::RunningUnknown { detail, .. } => R::RunningUnknown {
            kind: kind.into(),
            detail,
        },
        PrivacyUndoError::Changed(path) => R::ChangedSinceClean {
            path: path.display().to_string(),
        },
        PrivacyUndoError::Backup(detail) => R::Backup { detail },
        PrivacyUndoError::Store(detail) => R::UndoStore { detail },
        // Some files may be back already: not a refusal.
        PrivacyUndoError::Failed(detail) => return FfiError::Operation { detail },
    };
    refused(refusal)
}

impl From<PrivacyWriteReport> for FfiBrowserPrivacyReport {
    fn from(report: PrivacyWriteReport) -> Self {
        let mut skipped_cookies = FfiPrivacySkips::default();
        for skip in &report.skipped_cookies {
            skipped_cookies.count(skip.reason);
        }
        let mut skipped_history = FfiPrivacySkips::default();
        for skip in &report.skipped_history {
            skipped_history.count(skip.reason);
        }
        Self {
            profile_id: report.profile_id,
            removed_cookies: report.removed_cookies,
            removed_history_entries: report.removed_history_entries,
            removed_leftovers: report.removed_leftovers,
            skipped_cookies,
            skipped_history,
            backup_path: report.backup.map(|p| p.display().to_string()),
            undo_id: report.undo_id,
            issues: report.issues,
        }
    }
}

impl FfiPrivacySkips {
    fn count(&mut self, reason: SkipReason) {
        match reason {
            SkipReason::NowKept => self.now_kept += 1,
            SkipReason::NotFound => self.not_found += 1,
        }
    }
}

impl From<PrivacyUndoReport> for FfiBrowserUndoReport {
    fn from(report: PrivacyUndoReport) -> Self {
        Self {
            profile_id: report.profile_id,
            restored_files: report.restored.len() as u64,
            trashed_files: report.trashed.len() as u64,
        }
    }
}
