//! The Browsers page's read side: installed browsers, their profiles, each
//! profile's privacy counts and extensions, in one read-only scan. Cache
//! sizes are not here: they are Clean's `browser-caches` and
//! `browser-offline-data` items, whose `FfiEvidence::BrowserProfile`
//! carries the browser kind and profile id to file them under a profile.
//!
//! The scan never fails for a browser it cannot read: a refused or broken
//! file becomes an `FfiBrowserIssue` on that browser, profile or part
//! (`PermissionDenied` asks for Full Disk Access), and the rest is listed.
//!
//! Privacy consent, for the write call that follows (task 05 of the
//! Browsers plan): each scan stores every profile's full `ProfilePrivacy`,
//! removal lists included, under a new `generation`, replacing the previous
//! scan. Swift sees only the counts. The write takes the generation, a
//! browser kind with a profile id, and the parts to clean (cookies,
//! history), looks the profile up with `BrowsersState::consented`, and
//! deletes exactly the stored `cookies_to_remove` / `history_to_remove`
//! from the stored `cookie_store` / `history_store`: never a key, row or
//! path Swift sends, and never a fresh scan's list the user did not see.
//! A stale generation is `PreviewInvalid`. The write must also claim the
//! gate with `begin()`, so it cannot overlap a Browsers scan, and must
//! re-check that the browser is not running.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tiny_core::browsers::privacy::{self, ProfilePrivacy};
use tiny_core::browsers::{self, extensions, Browser, BrowserKind, BrowserProfile};
use tiny_core::clean::process::{AppProbe, RunnerProbe};
use tiny_core::progress::Progress;

use crate::clean::app_runner;
use crate::session::{CancellationToken, ScanSlot};
use crate::{FfiError, ProgressListener, TinySession};

mod records;
#[cfg(test)]
mod tests;

pub use records::*;

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
#[cfg_attr(not(test), allow(dead_code))] // Read by the privacy write (next task).
pub(crate) struct StoredProfile {
    pub(crate) profile: BrowserProfile,
    pub(crate) privacy: ProfilePrivacy,
}

impl BrowsersState {
    /// The profile `generation` scanned, as the consent basis of a privacy
    /// write: `PreviewInvalid` when a newer scan replaced it or none ran,
    /// `InvalidInput` for a profile that scan did not list.
    #[cfg_attr(not(test), allow(dead_code))] // Taken by the privacy write (next task).
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
        let key = ProfileKey {
            kind,
            profile_id: profile_id.to_string(),
        };
        scan.profiles
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
    /// profile's name, account, avatar, privacy counts and extensions.
    /// Read-only: cookie and history databases are read from copies, so a
    /// running browser is scanned too. Holds the Browsers scan slot of the
    /// operation gate (`Busy` while a write or another Browsers scan runs).
    /// Replaces the previous Browsers scan; a cancelled scan changes nothing.
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
                let privacy = privacy::scan_profile(browser.kind, profile, now);
                let listed = extensions::list(browser.kind, profile, now_unix);
                profiles.push(profile_record(profile, &privacy, &listed));
                stored.insert(
                    ProfileKey {
                        kind: browser.kind,
                        profile_id: profile.id.clone(),
                    },
                    StoredProfile {
                        profile: profile.clone(),
                        privacy,
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

fn profile_record(
    profile: &BrowserProfile,
    privacy: &ProfilePrivacy,
    listed: &extensions::ProfileExtensions,
) -> FfiBrowserProfile {
    FfiBrowserProfile {
        id: profile.id.clone(),
        name: profile.name.clone(),
        user_name: profile.user_name.clone(),
        avatar: (&profile.avatar).into(),
        highlight_color: profile.highlight_color,
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
