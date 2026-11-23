//! Undo of a Privacy clean: puts the recorded originals back, byte for
//! byte, from the backup folder in the Trash.
//!
//! Undo applies to a profile's latest write until its next scan (B25), and
//! only while every recorded file is still exactly what the write left: a
//! browser that ran since and changed a database is not rolled back over.
//! It refuses while any process of the browser runs. Every check runs
//! before the first file is touched. Every file to put back is first
//! copied beside itself and checked; only then does a file the write
//! created where none was move to the Trash, and each copy is renamed into
//! place. The backup stays in the Trash, and the record is consumed only
//! when every file hashes as its original again. An Undo that stopped
//! partway (a full disk, a failed rename) can be run again: a file that
//! already hashes as its original counts as put back.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::files::{self, in_a_trash};
use super::undo_store::{UndoStore, UndoStoreError};
use super::write::ensure_quit;
use super::write_types::{PrivacyEnv, PrivacyWriteError};
use super::{scan_profile, ProfilePrivacy};
use crate::browsers::{BrowserKind, BrowserProfile};

/// What an Undo put back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyUndoReport {
    pub profile_id: String,
    /// Files whose original bytes are back in place.
    pub restored: Vec<PathBuf>,
    /// Files the write had created, now in the Trash.
    pub trashed: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivacyUndoError {
    #[error("there is nothing to undo for that clean")]
    NoRecord,
    #[error("the profile was scanned again since that clean, so it cannot be undone")]
    Expired,
    #[error("that clean did not finish, so it cannot be undone; its backup is in the Trash")]
    Unfinished,
    #[error("{0} is running; quit it to undo")]
    Running(String),
    #[error("could not tell whether {browser} is running: {detail}")]
    RunningUnknown { browser: String, detail: String },
    #[error("{} changed since the clean, so it was not put back", .0.display())]
    Changed(PathBuf),
    #[error("the backup cannot be used: {0}")]
    Backup(String),
    #[error("{0}")]
    Store(String),
    #[error("Undo stopped partway: {0}")]
    Failed(String),
}

impl From<UndoStoreError> for PrivacyUndoError {
    fn from(error: UndoStoreError) -> Self {
        PrivacyUndoError::Store(error.0)
    }
}

/// Scans `profile` as [`scan_profile`] does, after ending the Undo of any
/// earlier clean of it. Nothing is scanned when the store cannot record
/// that.
pub fn scan_profile_recorded(
    kind: BrowserKind,
    profile: &BrowserProfile,
    now: SystemTime,
    store: &UndoStore,
) -> Result<ProfilePrivacy, UndoStoreError> {
    store.note_scan(kind, profile)?;
    Ok(scan_profile(kind, profile, now))
}

/// Undoes the clean `undo_id` of `profile`, as the module explains.
pub fn undo_privacy(
    kind: BrowserKind,
    profile: &BrowserProfile,
    undo_id: &str,
    env: &PrivacyEnv<'_>,
) -> Result<PrivacyUndoReport, PrivacyUndoError> {
    let lock = env.store.lock()?;
    let mut document = lock.load()?;
    let entry = document
        .entry(kind, profile)
        .ok_or(PrivacyUndoError::NoRecord)?;
    let record = entry
        .record
        .clone()
        .filter(|r| r.id == undo_id)
        .ok_or(PrivacyUndoError::NoRecord)?;
    if record.generation != entry.generation {
        document.entry_mut(kind, profile).record = None;
        lock.save(&document)?;
        return Err(PrivacyUndoError::Expired);
    }
    if !record.finished {
        return Err(PrivacyUndoError::Unfinished);
    }
    quit(env, kind)?;
    // Each file is what the write left, or already its original again
    // after an earlier Undo that stopped partway.
    let check_all = || -> Result<(), PrivacyUndoError> {
        for file in &record.files {
            let now = files::content_of(&file.path).map_err(PrivacyUndoError::Backup)?;
            if now != file.written && now != file.original {
                return Err(PrivacyUndoError::Changed(file.path.clone()));
            }
        }
        Ok(())
    };
    check_all()?;
    if !in_a_trash(&record.backup) {
        return Err(PrivacyUndoError::Backup(format!(
            "{} is not in the Trash",
            record.backup.display()
        )));
    }
    for file in record.files.iter().filter(|f| f.original.is_some()) {
        let copy = record.backup.join(&file.backup_name);
        if files::content_of(&copy).map_err(PrivacyUndoError::Backup)? != file.original {
            return Err(PrivacyUndoError::Backup(format!(
                "the copy of {} in the Trash is missing or changed",
                file.path.display()
            )));
        }
    }
    // Once more right before the first change.
    quit(env, kind)?;
    check_all()?;

    let mut report = PrivacyUndoReport {
        profile_id: profile.id.clone(),
        ..PrivacyUndoReport::default()
    };
    // Every copy is written and checked before anything is put back, so a
    // full disk stops the Undo with nothing changed.
    let mut staged = Vec::new();
    for file in &record.files {
        let Some(original) = &file.original else {
            continue;
        };
        let now = files::content_of(&file.path).map_err(PrivacyUndoError::Failed)?;
        if now.as_ref() == Some(original) {
            continue;
        }
        let copy =
            files::stage_restore(&record.backup.join(&file.backup_name), &file.path, original)
                .map_err(|e| partway(&report, &file.path, &e))?;
        staged.push((file.path.clone(), copy));
    }
    // Files the write created go next, so no stale journal sits beside a
    // restored database.
    for file in &record.files {
        if file.original.is_none() && file.written.is_some() {
            if files::content_of(&file.path)
                .map_err(PrivacyUndoError::Failed)?
                .is_none()
            {
                continue;
            }
            env.trash
                .move_to_trash(&file.path)
                .map_err(|e| partway(&report, &file.path, &e.to_string()))?;
            report.trashed.push(file.path.clone());
        }
    }
    for (path, copy) in staged {
        copy.commit().map_err(|e| partway(&report, &path, &e))?;
        report.restored.push(path);
    }
    for file in &record.files {
        let now = files::content_of(&file.path).map_err(PrivacyUndoError::Failed)?;
        if now != file.original {
            return Err(PrivacyUndoError::Failed(format!(
                "{} does not match its original after the restore",
                file.path.display()
            )));
        }
    }
    document.entry_mut(kind, profile).record = None;
    lock.save(&document)?;
    Ok(report)
}

fn quit(env: &PrivacyEnv<'_>, kind: BrowserKind) -> Result<(), PrivacyUndoError> {
    ensure_quit(env.processes, kind).map_err(|error| match error {
        PrivacyWriteError::RunningUnknown { browser, detail } => {
            PrivacyUndoError::RunningUnknown { browser, detail }
        }
        _ => PrivacyUndoError::Running(kind.display_name().into()),
    })
}

fn partway(report: &PrivacyUndoReport, path: &Path, detail: &str) -> PrivacyUndoError {
    PrivacyUndoError::Failed(format!(
        "{}: {detail} ({} restored, {} moved to the Trash before it)",
        path.display(),
        report.restored.len(),
        report.trashed.len()
    ))
}
