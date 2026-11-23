//! The Privacy clean: removes the cookies and history rows a caller
//! consents to from one Chromium or Firefox profile, after a backup.
//!
//! In order, holding the Undo store's lock throughout:
//!
//! 1. Safari is refused (B18, B20); so is a running browser ([`super::running`]).
//! 2. Every file the edit may touch is hashed: each database with its
//!    `-journal`, `-wal` and `-shm`, and Chromium's `Visited Links`.
//! 3. The profile is scanned again and the consent is intersected with what
//!    the scan lists now. A key or row no longer listed is skipped and
//!    reported; nothing outside the consent is ever added.
//! 4. The schema is checked on copies; an unknown one refuses.
//! 5. The files are copied into a backup folder, which moves to the Trash;
//!    every copy must hash as its original, there and in the Trash. An
//!    unfinished Undo record naming them is saved.
//! 6. Right before the edit: the browser must still be quit and every file
//!    must still hash as it did in step 2, so the backup is what is edited.
//! 7. One transaction edits the databases ([`super::edit`]); `Visited Links`,
//!    a file Chromium rebuilds, moves to the Trash instead of being edited.
//! 8. The files are hashed again and the record is finished with them.
//!
//! A failure before step 7 changes no browser file. An edit that fails
//! before its commit changed nothing either: the unfinished record is
//! dropped (the backup stays in the Trash) and the write is refused as
//! `Running` or `Changed`, so the consent can be used again. Only after a
//! failed commit, which may have changed a file, is a record kept whenever
//! some file did change, so Undo still applies.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::edit;
use super::files::{self, folder_name, in_a_trash, with_suffix, BackupEntry, Content};
use super::running::BrowserProcesses;
use super::schema::{self, Db, Role};
use super::undo_store::{RecordedFile, UndoRecord};
use super::write_scan::{intersect, Targets};
use super::write_types::{
    PrivacyConsent, PrivacyEnv, PrivacyWriteError, PrivacyWriteReport, SkipReason, SkippedCookie,
    SkippedHistory,
};
use super::{present, scan_profile, Stores};
use crate::browsers::{BrowserFamily, BrowserKind, BrowserProfile};
use crate::clean::finder_trash::Trash;

const SIBLING_SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];
const VISITED_LINKS: &str = "Visited Links";
const LEFTOVER_DBS: [(Role, &str); 3] = [
    (Role::Favicons, "Favicons"),
    (Role::TopSites, "Top Sites"),
    (Role::Shortcuts, "Shortcuts"),
];

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// The files of one part of the edit.
struct Part {
    dbs: Vec<Db>,
    /// Each database with its siblings, then `Visited Links`.
    files: Vec<PathBuf>,
    /// Files that move to the Trash after the edit.
    drop: Vec<PathBuf>,
}

impl Part {
    fn of(dbs: Vec<Db>, drop: Vec<PathBuf>) -> Self {
        let mut files = Vec::new();
        for db in &dbs {
            files.push(db.path.clone());
            files.extend(SIBLING_SUFFIXES.iter().map(|s| with_suffix(&db.path, s)));
        }
        files.extend(drop.iter().cloned());
        Part { dbs, files, drop }
    }
}

fn cookie_part(stores: &Stores) -> Option<Part> {
    let path = stores.cookies.clone()?;
    let role = match stores.family {
        BrowserFamily::Gecko => Role::FirefoxCookies,
        _ => Role::ChromiumCookies,
    };
    Some(Part::of(vec![Db { role, path }], Vec::new()))
}

fn history_part(stores: &Stores, dir: &Path) -> Option<Part> {
    let path = stores.history.clone()?;
    if stores.family == BrowserFamily::Gecko {
        return Some(Part::of(
            vec![Db {
                role: Role::Places,
                path,
            }],
            Vec::new(),
        ));
    }
    let mut dbs = vec![Db {
        role: Role::ChromiumHistory,
        path,
    }];
    dbs.extend(
        LEFTOVER_DBS
            .iter()
            .filter_map(|&(role, name)| present(dir.join(name)).map(|path| Db { role, path })),
    );
    let drop = present(dir.join(VISITED_LINKS)).into_iter().collect();
    Some(Part::of(dbs, drop))
}

/// Removes the consented cookies and history rows from `profile`, as the
/// module explains. `now` is the moment the 90 days count back from.
pub fn remove_privacy(
    kind: BrowserKind,
    profile: &BrowserProfile,
    consent: &PrivacyConsent,
    env: &PrivacyEnv<'_>,
    now: SystemTime,
) -> Result<PrivacyWriteReport, PrivacyWriteError> {
    let stores = match kind.family() {
        BrowserFamily::Chromium => Stores::chromium(&profile.data_dir),
        BrowserFamily::Gecko => Stores::firefox(&profile.data_dir),
        BrowserFamily::WebKit => {
            return Err(PrivacyWriteError::NotSupported(kind.display_name().into()))
        }
    };
    let lock = env
        .store
        .lock()
        .map_err(|e| PrivacyWriteError::Store(e.0))?;
    ensure_quit(env.processes, kind)?;

    let cookies = cookie_part(&stores);
    let history = history_part(&stores, &profile.data_dir);
    let mut before: HashMap<PathBuf, Content> = HashMap::new();
    for path in cookies.iter().chain(&history).flat_map(|p| &p.files) {
        before.insert(
            path.clone(),
            files::content_of(path).map_err(PrivacyWriteError::Backup)?,
        );
    }

    let scan = scan_profile(kind, profile, now);
    if let Some(issue) = scan.reason.clone().filter(|_| !scan.supported) {
        return Err(PrivacyWriteError::Unsupported(issue));
    }
    if scan.cookie_store != stores.cookies || scan.history_store != stores.history {
        return Err(PrivacyWriteError::Changed(
            "the profile's databases moved".into(),
        ));
    }
    let Targets {
        cookies: cookie_targets,
        history: history_targets,
        mut report,
    } = intersect(&stores, &scan, consent).map_err(PrivacyWriteError::Unsupported)?;
    report.profile_id = profile.id.clone();
    let parts: Vec<Part> = [
        cookies.filter(|_| !cookie_targets.is_empty()),
        history.filter(|_| !history_targets.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return Ok(report);
    }
    let dbs: Vec<Db> = parts.iter().flat_map(|p| p.dbs.iter().cloned()).collect();
    schema::check_copies(&dbs).map_err(PrivacyWriteError::Unsupported)?;

    // The backup, in the Trash, before anything is written.
    let paths: Vec<&PathBuf> = parts.iter().flat_map(|p| &p.files).collect();
    let mut recorded = Vec::with_capacity(paths.len());
    let mut names = BTreeSet::new();
    for path in &paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| names.insert(n.clone()))
            .ok_or_else(|| {
                PrivacyWriteError::Backup(format!("{} has no unique name", path.display()))
            })?;
        let original = before[*path].clone();
        recorded.push(RecordedFile {
            path: (*path).clone(),
            backup_name: name,
            written: original.clone(),
            original,
        });
    }
    let id = new_id();
    let mut document = lock.load().map_err(|e| PrivacyWriteError::Store(e.0))?;
    let label = format!(
        "TinySift {} backup, {}, {id}",
        kind.display_name(),
        profile.name
    );
    let staging = lock.staging(&folder_name(&label));
    let backup = back_up(&recorded, &staging, env.trash).map_err(PrivacyWriteError::Backup)?;
    let (generation, previous) = document
        .entry(kind, profile)
        .map_or((0, None), |e| (e.generation, e.record.clone()));
    let mut record = UndoRecord {
        id: id.clone(),
        generation,
        backup: backup.clone(),
        files: recorded,
        finished: false,
    };
    document.entry_mut(kind, profile).record = Some(record.clone());
    lock.save(&document)
        .map_err(|e| PrivacyWriteError::Store(e.0))?;
    // Nothing changed: the earlier clean's Undo, if any, still applies.
    let forget = |document: &mut super::undo_store::Document| {
        document.entry_mut(kind, profile).record = previous.clone();
        // Best effort: a record left unfinished is one Undo refuses.
        let _ = lock.save(document);
    };

    // Right before the edit.
    if let Err(error) = ensure_quit(env.processes, kind).and_then(|()| unchanged(&record, &before))
    {
        forget(&mut document);
        return Err(error);
    }
    let edited = match edit::apply(&dbs, &cookie_targets, &history_targets) {
        // Nothing of ours was committed (a browser that started holds the
        // lock, say): whatever changed since is the browser's, so no record
        // is finished over it, and the consent stays for another try.
        Err(edit::EditError::BeforeCommit(issue)) => {
            forget(&mut document);
            return Err(ensure_quit(env.processes, kind)
                .err()
                .unwrap_or_else(|| PrivacyWriteError::Changed(issue_text(&issue))));
        }
        other => other.map_err(edit::EditError::issue),
    };
    let mut issues = Vec::new();
    if edited.is_ok() {
        for path in parts.iter().flat_map(|p| &p.drop) {
            if before[path].is_some() {
                if let Err(e) = env.trash.move_to_trash_reporting(path) {
                    issues.push(format!("{} stayed in place: {e}", path.display()));
                }
            }
        }
    }

    // The record of what the edit left.
    let after = files::contents(record.files.iter().map(|f| f.path.as_path()));
    let changed = match &after {
        Ok(after) => record
            .files
            .iter()
            .zip(after)
            .any(|(f, a)| &f.original != a),
        Err(_) => true,
    };
    let undo_id = match after {
        Ok(after) if changed => {
            for (file, content) in record.files.iter_mut().zip(after) {
                file.written = content;
            }
            record.finished = true;
            document.entry_mut(kind, profile).record = Some(record);
            match lock.save(&document) {
                Ok(()) => Some(id),
                Err(e) => {
                    issues.push(format!("Undo is not available: {e}"));
                    None
                }
            }
        }
        Ok(_) => {
            forget(&mut document);
            None
        }
        Err(e) => {
            issues.push(format!("Undo is not available: {e}"));
            None
        }
    };
    let removed = edited.map_err(|issue| PrivacyWriteError::Write {
        detail: issue.detail,
        undo_id: undo_id.clone(),
    })?;
    report.removed_cookies = removed.cookie_rows;
    report.removed_history_entries = removed.history_rows;
    report.removed_leftovers = removed.leftover_rows;
    report
        .skipped_cookies
        .extend(removed.cookies_gone.into_iter().map(|key| SkippedCookie {
            key,
            reason: SkipReason::NotFound,
        }));
    report.skipped_history.extend(
        (removed
            .history_gone
            .into_iter()
            .map(|id| (id, SkipReason::NotFound)))
        .chain(
            removed
                .history_bookmarked
                .into_iter()
                .map(|id| (id, SkipReason::NowKept)),
        )
        .map(|(id, reason)| SkippedHistory { id, reason }),
    );
    report.backed_up = paths
        .iter()
        .filter(|p| before[**p].is_some())
        .map(|p| (*p).clone())
        .collect();
    report.backup = Some(backup);
    report.undo_id = undo_id;
    report.issues = issues;
    Ok(report)
}

/// Copies the present files into `staging`, moves it to the Trash and
/// checks the copies there. Returns the folder's place in the Trash.
fn back_up(files: &[RecordedFile], staging: &Path, trash: &dyn Trash) -> Result<PathBuf, String> {
    let entries: Vec<BackupEntry<'_>> = files
        .iter()
        .map(|f| BackupEntry {
            path: &f.path,
            name: &f.backup_name,
            content: &f.original,
        })
        .collect();
    files::stage_backup(&entries, staging)?;
    let moved = match trash.move_to_trash_reporting(staging) {
        Ok(moved) => moved,
        Err(e) => {
            // The folder is this write's own; it never reached the Trash.
            let _ = std::fs::remove_dir_all(staging);
            return Err(format!("could not move the backup to the Trash: {e}"));
        }
    };
    let Some(moved) = moved.filter(|p| in_a_trash(p)) else {
        return Err("the Trash did not say where the backup went".into());
    };
    files::verify_backup(&entries, &moved)?;
    Ok(moved)
}

fn issue_text(issue: &super::PrivacyIssue) -> String {
    match &issue.path {
        Some(path) => format!("{}: {}", path.display(), issue.detail),
        None => issue.detail.clone(),
    }
}

/// Every recorded file still hashes as it did before the scan.
fn unchanged(
    record: &UndoRecord,
    before: &HashMap<PathBuf, Content>,
) -> Result<(), PrivacyWriteError> {
    for file in &record.files {
        let now = files::content_of(&file.path).map_err(PrivacyWriteError::Changed)?;
        if now != before[&file.path] {
            return Err(PrivacyWriteError::Changed(format!(
                "{} changed",
                file.path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn ensure_quit(
    processes: &dyn BrowserProcesses,
    kind: BrowserKind,
) -> Result<(), PrivacyWriteError> {
    match processes.running(kind) {
        Ok(false) => Ok(()),
        Ok(true) => Err(PrivacyWriteError::Running(kind.display_name().into())),
        Err(detail) => Err(PrivacyWriteError::RunningUnknown {
            browser: kind.display_name().into(),
            detail,
        }),
    }
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{nanos:x}-{:x}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}
