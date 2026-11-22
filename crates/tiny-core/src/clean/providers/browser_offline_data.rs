//! `browser-offline-data`: each Chromium profile's
//! `Service Worker/CacheStorage`, the copies sites keep for offline use.
//! Its own category, apart from `browser-caches`, because it can hold
//! files the user saved for offline reading, which no site downloads
//! again: Review, never ticked ahead by the app, and only to the Trash
//! (`comes_back` is `TrashOnly`).

use std::path::{Path, PathBuf};

use crate::browsers::{discover, BrowserFamily};
use crate::error::Result;

use super::browser_caches::{gate_apps, offer, profile_space_folders, ProfileFolder, SpacePart};
use super::{execute_per_item, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "browser-offline-data";
const LABEL: &str = "Offline site data";
/// Inside a Chromium profile's data folder.
pub(super) const CACHE_STORAGE: &str = "Service Worker/CacheStorage";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Every Chromium profile's `Service Worker/CacheStorage` under `home`.
pub(super) fn offline_folders(home: &Path) -> Vec<ProfileFolder> {
    discover(home)
        .into_iter()
        .filter(|browser| browser.family == BrowserFamily::Chromium)
        .flat_map(|browser| {
            let kind = browser.kind;
            browser.profiles.into_iter().flat_map(move |profile| {
                profile_space_folders(home, kind, &profile)
                    .into_iter()
                    .filter(|f| f.part == SpacePart::Offline)
                    .map(|f| ProfileFolder {
                        path: f.path,
                        kind,
                        profile_id: profile.id.clone(),
                        profile_name: profile.name.clone(),
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

pub struct BrowserOfflineData;

impl BrowserOfflineData {
    pub(super) fn discover_in(&self, ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
        offer(ctx, offline_folders(home), ID, LABEL)
    }
}

impl CleanProvider for BrowserOfflineData {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Offline copies sites keep through service workers; sites download them again, but files saved for offline use go with them".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn item_apps(&self, path: &Path) -> Vec<String> {
        gate_apps(path)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(match home() {
            Some(h) => self.discover_in(ctx, &h),
            None => Vec::new(),
        })
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}
