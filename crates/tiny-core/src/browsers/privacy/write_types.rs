//! What a Privacy clean takes, uses and reports: the consent, the
//! environment it runs in, its report and its errors.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::running::BrowserProcesses;
use super::undo_store::UndoStore;
use super::{CookieKey, PrivacyIssue};
use crate::clean::finder_trash::Trash;

/// What the caller consents to remove: keys and row ids from the scan's
/// [`super::ProfilePrivacy::cookies_to_remove`] and
/// [`super::ProfilePrivacy::history_to_remove`]. Only these can move.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyConsent {
    pub cookies: Vec<CookieKey>,
    pub history: Vec<i64>,
}

/// The running check, the Trash and the Undo store a write and an Undo use.
pub struct PrivacyEnv<'a> {
    pub processes: &'a dyn BrowserProcesses,
    pub trash: &'a dyn Trash,
    pub store: &'a UndoStore,
}

/// Why a consented item was left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkipReason {
    /// The profile no longer holds it.
    NotFound,
    /// It counts as kept now: its site was visited or the cookie used since
    /// the scan, or a bookmark points at the history row.
    NowKept,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedCookie {
    pub key: CookieKey,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedHistory {
    pub id: i64,
    pub reason: SkipReason,
}

/// What a Privacy clean did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyWriteReport {
    pub profile_id: String,
    /// Cookie rows removed (a key may cover several rows).
    pub removed_cookies: u64,
    /// History entries removed: Chromium `urls` or Firefox `moz_places` rows.
    pub removed_history_entries: u64,
    /// Favicon mappings, top sites and shortcuts removed with them.
    pub removed_leftovers: u64,
    pub skipped_cookies: Vec<SkippedCookie>,
    pub skipped_history: Vec<SkippedHistory>,
    /// The backup folder in the Trash; `None` when nothing was to remove.
    pub backup: Option<PathBuf>,
    /// The originals the backup holds a copy of.
    pub backed_up: Vec<PathBuf>,
    /// For [`super::undo_privacy`]; `None` when nothing changed or the
    /// record could not be finished (see `issues`).
    pub undo_id: Option<String>,
    /// Parts that did not complete after the edit itself succeeded.
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivacyWriteError {
    #[error("{0} privacy data is not edited in this build")]
    NotSupported(String),
    #[error("cannot edit {}: {}", .0.path.as_deref().unwrap_or(Path::new("")).display(), .0.detail)]
    Unsupported(PrivacyIssue),
    #[error("{0} is running; quit it to clean it")]
    Running(String),
    #[error("could not tell whether {browser} is running: {detail}")]
    RunningUnknown { browser: String, detail: String },
    #[error("the backup failed, so nothing was changed: {0}")]
    Backup(String),
    #[error("the Undo record could not be kept, so nothing was changed: {0}")]
    Store(String),
    #[error("the browser's files changed during the clean, so nothing was changed: {0}")]
    Changed(String),
    #[error("the edit failed: {detail}")]
    Write {
        detail: String,
        /// Set when some file did change; Undo puts it back.
        undo_id: Option<String>,
    },
}
