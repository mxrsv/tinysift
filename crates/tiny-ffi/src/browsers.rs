//! The Browsers page's read side: installed browsers, their profiles, each
//! profile's space, privacy counts and extensions, in one read-only scan.
//! The write side, Privacy clean and its Undo, is in `write`.
//!
//! Space is measured for display only: each profile's folders that
//! `browser-caches` and `browser-offline-data` offer, by their own path
//! rule (`profile_space_folders`) and Clean's size rule, a running
//! browser's included. Cleaning them still goes through Clean's items, which
//! leave a running browser's folders out; their `FfiEvidence::BrowserProfile`
//! carries the browser kind and profile id to file them under a profile.
//!
//! The scan never fails for a browser it cannot read: a refused or broken
//! file becomes an `FfiBrowserIssue` on that browser, profile or part
//! (`PermissionDenied` asks for Full Disk Access), and the rest is listed.
//! Each profile's privacy scan first ends the Undo of any earlier Privacy
//! clean of it (`scan_profile_recorded`, B25); when the Undo store cannot
//! record that, the profile's privacy is an issue and is not scanned.
//!
//! Privacy consent: each scan stores every profile's full `ProfilePrivacy`,
//! removal lists included, under a new `generation`, replacing the previous
//! scan. Swift sees only the counts. `browsers_clean_privacy` takes the
//! generation and a browser kind with a profile id, looks the profile up
//! with `BrowsersState::consented`, and consents to exactly the stored
//! `cookies_to_remove` and `history_to_remove`: never a key, row or path
//! Swift sends, and never a fresh scan's list the user did not see. Core
//! then takes only what a fresh scan of the same profile folder still
//! lists. A stale generation, or a profile already cleaned under it, is
//! `PreviewInvalid`. The write claims the gate with `begin()`, so it cannot
//! overlap a Browsers scan, and core re-checks that the browser is quit.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tiny_core::browsers::privacy::{
    self, PrivacyIssue, PrivacyIssueKind, ProfilePrivacy, UndoStore, UndoStoreError,
};
use tiny_core::browsers::{self, extensions, Browser, BrowserKind, BrowserProfile};
use tiny_core::clean::fs_safe::dir_size_checked;
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::clean::providers::browser_caches::{profile_space_folders, SpacePart};
use tiny_core::clean::scan_context::ScanContext;
use tiny_core::progress::Progress;

use crate::clean::app_runner;
use crate::session::{CancellationToken, ScanSlot};
use crate::{FfiError, ProgressListener, TinySession};

mod records;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_write;
mod write;

pub use records::*;
pub use write::*;

const OPERATION: &str = "browsers";

/// Session-held Browsers state: the last completed scan.
#[derive(Default)]
pub(crate) struct BrowsersState {
    generation: u64,
    scan: Option<StoredScan>,
}

/// What the last scan read, per profile, as the write will need it.
struct StoredScan {
    generation: u64,
    profiles: HashMap<ProfileKey, StoredProfile>,
}

/// A profile id is unique within its browser only.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProfileKey {
    kind: BrowserKind,
    profile_id: String,
}

/// One profile as scanned: its folders and the privacy result whose counts
/// Swift was shown.
#[derive(Debug, Clone)]
pub(crate) struct StoredProfile {
    pub(crate) profile: BrowserProfile,
    pub(crate) privacy: ProfilePrivacy,
    /// A Privacy clean already ran on this consent: it is spent.
    pub(crate) cleaned: bool,
}

impl BrowsersState {
    /// The profile `generation` scanned, as the consent basis of a privacy
    /// write: `PreviewInvalid` when a newer scan replaced it, none ran or a
    /// clean already spent it, `InvalidInput` for a profile that scan did
    /// not list.
    pub(crate) fn consented(
        &self,
        generation: u64,
        kind: BrowserKind,
        profile_id: &str,
    ) -> Result<&StoredProfile, FfiError> {
        let scan = self
            .scan
            .as_ref()
            .filter(|scan| scan.generation == generation)
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: "the browser scan was replaced; scan again".into(),
            })?;
        let stored = scan.profile(kind, profile_id)?;
        if stored.cleaned {
            return Err(FfiError::PreviewInvalid {
                detail: "that profile was already cleaned; scan again".into(),
            });
        }
        Ok(stored)
    }

    /// The profile as the last scan found it, whatever its generation: the
    /// folder an Undo puts files back in. Whether the Undo still applies is
    /// the Undo store's call (B25).
    pub(crate) fn scanned(
        &self,
        kind: BrowserKind,
        profile_id: &str,
    ) -> Result<&StoredProfile, FfiError> {
        self.scan
            .as_ref()
            .ok_or_else(|| FfiError::PreviewInvalid {
                detail: "no browser scan in this session; scan again".into(),
            })?
            .profile(kind, profile_id)
    }

    /// Marks the consent of `generation`'s profile as spent.
    pub(crate) fn spend(&mut self, generation: u64, kind: BrowserKind, profile_id: &str) {
        let key = ProfileKey {
            kind,
            profile_id: profile_id.to_string(),
        };
        if let Some(stored) = self
            .scan
            .as_mut()
            .filter(|scan| scan.generation == generation)
            .and_then(|scan| scan.profiles.get_mut(&key))
        {
            stored.cleaned = true;
        }
    }
}

impl StoredScan {
    fn profile(&self, kind: BrowserKind, profile_id: &str) -> Result<&StoredProfile, FfiError> {
        let key = ProfileKey {
            kind,
            profile_id: profile_id.to_string(),
        };
        self.profiles
            .get(&key)
            .ok_or_else(|| FfiError::InvalidInput {
                detail: format!(
                    "no {} profile `{profile_id}` in that scan",
                    kind.display_name()
                ),
            })
    }
}

#[uniffi::export]
impl TinySession {
    /// Lists the installed browsers under the user's home, with each
    /// profile's name, account, avatar, space, privacy counts and
    /// extensions. Read-only: cookie and history databases are read from
    /// copies, so a running browser is scanned too. Ends the Undo of any
    /// earlier Privacy clean of a scanned profile. Holds the Browsers scan
    /// slot of the operation gate (`Busy` while a write or another Browsers
    /// scan runs). Replaces the previous Browsers scan; a cancelled scan
    /// changes nothing it stores (a cancelled scan may still have ended an
    /// Undo).
    pub fn browsers_scan(
        &self,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiBrowsersReport, FfiError> {
        let home = self.clean_home()?;
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.scan_browsers(&home, &probe, SystemTime::now(), &token, progress.as_ref())
    }
}

impl TinySession {
    pub(crate) fn browsers_state(&self) -> MutexGuard<'_, BrowsersState> {
        self.browsers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `browsers_scan` with its home, running check and clock passed in
    /// (the test seam).
    pub(crate) fn scan_browsers(
        &self,
        home: &Path,
        probe: &dyn AppProbe,
        now: SystemTime,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiBrowsersReport, FfiError> {
        let _gate = self.begin_scan(ScanSlot::Browsers)?;
        let store = UndoStore::for_home(home);
        let ctx = ScanContext::new(Some(token.flag()), probe);
        let found = browsers::discover(home);
        let total = found.iter().map(|b| b.profiles.len() as u64).sum();
        let now_unix = now
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let mut completed = 0;
        let mut stored = HashMap::new();
        let mut records = Vec::with_capacity(found.len());
        for browser in &found {
            let mut profiles = Vec::with_capacity(browser.profiles.len());
            for profile in &browser.profiles {
                token.check()?;
                progress.on_progress(step(completed, total, &browser.name).into());
                let privacy = privacy::scan_profile_recorded(browser.kind, profile, now, &store)
                    .unwrap_or_else(|error| store_failed(profile, &store, &error));
                let listed = extensions::list(browser.kind, profile, now_unix);
                let space = measure(home, browser.kind, profile, &ctx);
                profiles.push(profile_record(profile, &privacy, &listed, space));
                stored.insert(
                    ProfileKey {
                        kind: browser.kind,
                        profile_id: profile.id.clone(),
                    },
                    StoredProfile {
                        profile: profile.clone(),
                        privacy,
                        cleaned: false,
                    },
                );
                completed += 1;
            }
            records.push(browser_record(browser, probe, profiles));
        }
        // A flag set after the last checkpoint still discards the scan.
        token.check()?;
        progress.on_progress(step(completed, total, "").into());
        let mut state = self.browsers_state();
        state.generation += 1;
        let generation = state.generation;
        state.scan = Some(StoredScan {
            generation,
            profiles: stored,
        });
        Ok(FfiBrowsersReport {
            generation,
            browsers: records,
        })
    }
}

fn step(completed: u64, total: u64, message: &str) -> Progress {
    Progress {
        operation: OPERATION.into(),
        completed,
        total: Some(total),
        message: message.into(),
        found_bytes: None,
        category: None,
        current_category: None,
    }
}

fn browser_record(
    browser: &Browser,
    probe: &dyn AppProbe,
    profiles: Vec<FfiBrowserProfile>,
) -> FfiBrowser {
    FfiBrowser {
        kind: browser.kind.into(),
        name: browser.name.clone(),
        bundle_id: browser.bundle_id.clone(),
        running: running(&browser.app_names, probe),
        profiles,
        issue: browser.issue.as_ref().map(Into::into),
    }
}

/// The profile's privacy when the Undo store could not record its scan:
/// not scanned, so an earlier clean's Undo is never left to apply over a
/// newer scan.
fn store_failed(
    profile: &BrowserProfile,
    store: &UndoStore,
    error: &UndoStoreError,
) -> ProfilePrivacy {
    ProfilePrivacy {
        profile_id: profile.id.clone(),
        supported: false,
        reason: Some(PrivacyIssue {
            kind: PrivacyIssueKind::Unreadable,
            path: Some(store.dir().to_path_buf()),
            detail: format!("the Undo store could not record the scan: {error}"),
        }),
        cookie_store: None,
        history_store: None,
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

/// The bytes of each part of the profile's cache folders, as Clean sizes
/// its items.
fn measure(
    home: &Path,
    kind: BrowserKind,
    profile: &BrowserProfile,
    ctx: &ScanContext<'_>,
) -> FfiProfileSpace {
    let mut space = FfiProfileSpace::default();
    for folder in profile_space_folders(home, kind, profile) {
        let bytes = dir_size_checked(&folder.path, ctx);
        let part = match folder.part {
            SpacePart::Cache => &mut space.cache_bytes,
            SpacePart::CodeCache => &mut space.code_cache_bytes,
            SpacePart::Other => &mut space.other_bytes,
            SpacePart::Offline => &mut space.offline_bytes,
        };
        *part = part.saturating_add(bytes);
    }
    space
}

fn profile_record(
    profile: &BrowserProfile,
    privacy: &ProfilePrivacy,
    listed: &extensions::ProfileExtensions,
    space: FfiProfileSpace,
) -> FfiBrowserProfile {
    FfiBrowserProfile {
        id: profile.id.clone(),
        name: profile.name.clone(),
        user_name: profile.user_name.clone(),
        avatar: (&profile.avatar).into(),
        highlight_color: profile.highlight_color,
        space,
        privacy: privacy.into(),
        extensions: listed.extensions.iter().map(Into::into).collect(),
        extensions_issue: listed.issue.as_ref().map(Into::into),
    }
}

/// Clean's running check on the browser's process names: running when any
/// name runs, unknown when none does and a check failed.
fn running(names: &[String], probe: &dyn AppProbe) -> Option<bool> {
    let mut unknown = false;
    for name in names {
        match probe.probe(name) {
            Ok(true) => return Some(true),
            Ok(false) => {}
            Err(_) => unknown = true,
        }
    }
    (!unknown).then_some(false)
}
