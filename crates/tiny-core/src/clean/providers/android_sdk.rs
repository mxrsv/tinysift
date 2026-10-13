use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, is_idle, root_as_item, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "android-sdk";
const LABEL: &str = "Android SDK caches";
const APP: &str = "Android Studio";

// Always-safe roots (cache + temp, both under user home).
const SAFE_DIRS: &[&str] = &[".android/cache", ".gradle/.tmp"];

// Conditional: system-images, only when present + idle.
const SYSTEM_IMAGES: &str = "Library/Android/sdk/system-images";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct AndroidSdk {
    pub idle_days: u64,
}

impl AndroidSdk {
    pub fn new(idle_days: u64) -> Self {
        Self { idle_days }
    }
}

impl CleanProvider for AndroidSdk {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        format!("Android caches the IDE re-downloads, plus emulator system images untouched for {} days", self.idle_days)
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(APP)
    }
    fn available(&self) -> bool {
        let h = match home() {
            Some(h) => h,
            None => return false,
        };
        SAFE_DIRS.iter().any(|s| h.join(s).exists()) || h.join(SYSTEM_IMAGES).exists()
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let mut items = Vec::new();
        for sub in SAFE_DIRS {
            items.extend(root_as_item(
                ctx,
                &h.join(sub),
                ID,
                LABEL,
                RiskLevel::Review,
            ));
        }
        // system-images is huge — only flag if present AND idle.
        let sys = h.join(SYSTEM_IMAGES);
        if sys.is_dir() && is_idle(&sys, self.idle_days) {
            items.extend(root_as_item(ctx, &sys, ID, LABEL, RiskLevel::Review));
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;

    #[test]
    fn android_sdk_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn android_sdk_gates_studio() {
        let p = AndroidSdk::new(30);
        assert_eq!(p.requires_app_quit(), Some("Android Studio"));
    }
}
