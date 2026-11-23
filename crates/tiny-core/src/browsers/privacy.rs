//! Cookies and history a profile could lose, counted read-only on copies.
//!
//! [`scan_profile`] copies a profile's cookie and history databases, with
//! their `-journal`, `-wal` and `-shm` files, into a private temporary
//! folder and reads only the copies, so the browser's own files are never
//! opened, locked or changed. A browser may be running while it is scanned.
//!
//! Each cookie belongs to a site, its registrable domain (eTLD+1) on the
//! Public Suffix List. With `cutoff` being [`OLD_AFTER_DAYS`] before `now`:
//!
//! - **Kept**: the site has a history visit after the cutoff, or the
//!   cookie itself was used after it.
//! - **Tracker**: otherwise, when the site never appears in the history.
//! - **Old**: otherwise.
//!
//! A profile with no history visits at all classifies no cookie by
//! history: a fresh or wiped profile would otherwise call every cookie,
//! the signed-in account's included, a tracker. There only a recorded last
//! use before the cutoff makes a cookie old, and none is a tracker.
//!
//! History entries whose last visit is before the cutoff are old, except
//! Firefox rows that a bookmark or keyword points at (`foreign_count > 0`).
//!
//! Safari is not scanned in this build: its cookie store is not yet known
//! to be the only one, and its history syncs through iCloud.
//!
//! The write step deletes exactly [`ProfilePrivacy::cookies_to_remove`] and
//! [`ProfilePrivacy::history_to_remove`]. A cookie key is listed only when
//! no row it matches is kept, so deleting by the key never takes a kept
//! cookie.

mod classify;
mod copy;
mod edit;
mod files;
mod running;
mod schema;
mod sha256;
mod site;
mod stores;
mod undo;
mod undo_store;
mod write;
mod write_scan;
mod write_types;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_refuse;
#[cfg(test)]
mod tests_store;
#[cfg(test)]
mod tests_undo;
#[cfg(test)]
mod tests_write;
#[cfg(test)]
mod tests_write_fixture;

pub use running::{BrowserProcesses, LiveBundles, RunningBundles, RunningCheck};
pub use undo::{scan_profile_recorded, undo_privacy, PrivacyUndoError, PrivacyUndoReport};
pub use undo_store::{UndoStore, UndoStoreError};
pub use write::remove_privacy;
pub use write_types::{
    PrivacyConsent, PrivacyEnv, PrivacyWriteError, PrivacyWriteReport, SkipReason, SkippedCookie,
    SkippedHistory,
};

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Browser, BrowserFamily, BrowserKind, BrowserProfile};
use copy::Snapshot;

/// Days without a visit or use after which a site's cookies and history
/// count as old.
pub const OLD_AFTER_DAYS: i64 = 90;

const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;

/// What the Privacy part of one profile would remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePrivacy {
    /// [`BrowserProfile::id`] of the scanned profile.
    pub profile_id: String,
    /// False when the profile could not be scanned; `reason` says why and
    /// every count is zero.
    pub supported: bool,
    pub reason: Option<PrivacyIssue>,
    /// The cookie database read, when the profile has one.
    pub cookie_store: Option<PathBuf>,
    /// The history database read, when the profile has one.
    pub history_store: Option<PathBuf>,
    /// Cookies, counted by row.
    pub trackers: u64,
    pub old: u64,
    pub kept: u64,
    /// Distinct sites among the tracker and the old cookies.
    pub tracker_sites: u64,
    pub old_sites: u64,
    pub old_history_entries: u64,
    /// No history entry carries a visit, or there is no history database;
    /// no cookie was then classified by history.
    pub history_is_empty: bool,
    /// Sorted by host, name, path.
    pub cookies_to_remove: Vec<CookieKey>,
    /// Row ids in Chromium's `urls` or Firefox's `moz_places`, ascending.
    pub history_to_remove: Vec<i64>,
}

/// The columns that pick out a cookie's rows: Chromium `host_key`, `name`,
/// `path`; Firefox `host`, `name`, `path` and `originAttributes` (when the
/// table has that column).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieKey {
    /// As stored, with any leading dot.
    pub host: String,
    pub name: String,
    pub path: String,
    pub origin_attributes: Option<String>,
    pub class: RemovedCookie,
}

/// Why a cookie is listed for removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemovedCookie {
    Tracker,
    Old,
}

/// Why a profile's privacy data was not scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyIssue {
    pub kind: PrivacyIssueKind,
    /// The database that failed; `None` when the browser is not scanned.
    pub path: Option<PathBuf>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrivacyIssueKind {
    /// This build does not scan the browser's privacy data (Safari).
    NotSupported,
    /// A table or column the scan needs is missing or holds another type.
    UnknownSchema,
    /// macOS refused the read.
    PermissionDenied,
    /// The file could not be copied or is not a readable database.
    Unreadable,
}

impl PrivacyIssue {
    fn new(kind: PrivacyIssueKind, path: &Path, detail: impl Into<String>) -> Self {
        PrivacyIssue {
            kind,
            path: Some(path.to_path_buf()),
            detail: detail.into(),
        }
    }
}

/// Scans every profile of `browser`, in its order.
pub fn scan_browser(browser: &Browser, now: SystemTime) -> Vec<ProfilePrivacy> {
    browser
        .profiles
        .iter()
        .map(|profile| scan_profile(browser.kind, profile, now))
        .collect()
}

/// Counts what Privacy would remove from `profile`, reading only copies.
/// `now` is the moment the 90 days are counted back from.
pub fn scan_profile(
    kind: BrowserKind,
    profile: &BrowserProfile,
    now: SystemTime,
) -> ProfilePrivacy {
    let stores = match kind.family() {
        BrowserFamily::Chromium => Stores::chromium(&profile.data_dir),
        BrowserFamily::Gecko => Stores::firefox(&profile.data_dir),
        BrowserFamily::WebKit => {
            let issue = PrivacyIssue {
                kind: PrivacyIssueKind::NotSupported,
                path: None,
                detail: format!(
                    "{} privacy data is not scanned in this build",
                    kind.display_name()
                ),
            };
            return ProfilePrivacy::unsupported(profile, None, issue);
        }
    };
    let cutoff = unix_micros(now) - OLD_AFTER_DAYS * MICROS_PER_DAY;
    match scan_stores(&stores, cutoff) {
        Ok(tally) => tally.into_result(profile, stores),
        Err(issue) => ProfilePrivacy::unsupported(profile, Some(stores), issue),
    }
}

/// Where a profile keeps its two databases, and how to read them.
struct Stores {
    family: BrowserFamily,
    cookies: Option<PathBuf>,
    history: Option<PathBuf>,
}

impl Stores {
    /// Chromium moved `Cookies` into `Network/` (Chrome 96), yet Chrome 157
    /// on macOS still keeps it in the profile folder. With both present the
    /// one written last is live. A symlink is kept, so the copy step
    /// refuses it with a reason instead of the store looking absent.
    fn chromium(dir: &Path) -> Self {
        let cookies = [dir.join("Cookies"), dir.join("Network").join("Cookies")]
            .into_iter()
            .filter_map(|path| {
                let meta = path.symlink_metadata().ok()?;
                (!meta.is_dir()).then(|| (meta.modified().ok(), path))
            })
            .max_by_key(|(modified, _)| *modified)
            .map(|(_, path)| path);
        Stores {
            family: BrowserFamily::Chromium,
            cookies,
            history: present(dir.join("History")),
        }
    }

    fn firefox(dir: &Path) -> Self {
        Stores {
            family: BrowserFamily::Gecko,
            cookies: present(dir.join("cookies.sqlite")),
            history: present(dir.join("places.sqlite")),
        }
    }
}

/// The path when something is there. A symlink is kept so the copy step
/// refuses it with a reason instead of the store looking absent.
fn present(path: PathBuf) -> Option<PathBuf> {
    path.symlink_metadata().is_ok().then_some(path)
}

fn scan_stores(stores: &Stores, cutoff: i64) -> Result<classify::Tally, PrivacyIssue> {
    let history = match &stores.history {
        Some(path) => {
            let snapshot = Snapshot::of(path)?;
            match stores.family {
                BrowserFamily::Gecko => stores::firefox_history(snapshot.connection(), path)?,
                _ => stores::chromium_history(snapshot.connection(), path)?,
            }
        }
        None => Vec::new(),
    };
    let cookies = match &stores.cookies {
        Some(path) => {
            let snapshot = Snapshot::of(path)?;
            match stores.family {
                BrowserFamily::Gecko => stores::firefox_cookies(snapshot.connection(), path)?,
                _ => stores::chromium_cookies(snapshot.connection(), path)?,
            }
        }
        None => Vec::new(),
    };
    Ok(classify::tally(&cookies, &history, cutoff))
}

impl ProfilePrivacy {
    fn unsupported(profile: &BrowserProfile, stores: Option<Stores>, issue: PrivacyIssue) -> Self {
        let (cookie_store, history_store) = stores.map_or((None, None), |s| (s.cookies, s.history));
        ProfilePrivacy {
            profile_id: profile.id.clone(),
            supported: false,
            reason: Some(issue),
            cookie_store,
            history_store,
            trackers: 0,
            old: 0,
            kept: 0,
            tracker_sites: 0,
            old_sites: 0,
            old_history_entries: 0,
            history_is_empty: false,
            cookies_to_remove: Vec::new(),
            history_to_remove: Vec::new(),
        }
    }
}

impl classify::Tally {
    fn into_result(self, profile: &BrowserProfile, stores: Stores) -> ProfilePrivacy {
        ProfilePrivacy {
            profile_id: profile.id.clone(),
            supported: true,
            reason: None,
            cookie_store: stores.cookies,
            history_store: stores.history,
            trackers: self.trackers,
            old: self.old,
            kept: self.kept,
            tracker_sites: self.tracker_sites,
            old_sites: self.old_sites,
            old_history_entries: self.history_to_remove.len() as u64,
            history_is_empty: self.history_is_empty,
            cookies_to_remove: self.cookies_to_remove,
            history_to_remove: self.history_to_remove,
        }
    }
}

/// Microseconds since the Unix epoch; a moment before it counts as the
/// epoch.
fn unix_micros(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}
