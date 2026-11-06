use crate::error::Result;
use std::path::PathBuf;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const APP: &str = "Xcode";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct XcodeDerivedData;

const DERIVED_ID: &str = "xcode-derived";
const DERIVED_LABEL: &str = "Xcode DerivedData";

impl CleanProvider for XcodeDerivedData {
    fn id(&self) -> &'static str {
        DERIVED_ID
    }
    fn label(&self) -> &'static str {
        DERIVED_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Build output Xcode regenerates on the next build".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(APP)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let root = h.join("Library/Developer/Xcode/DerivedData");
        Ok(root_as_item(
            ctx,
            &root,
            DERIVED_ID,
            DERIVED_LABEL,
            RiskLevel::Safe,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, DERIVED_ID)
    }
}

pub struct XcodeArchives;

const ARCHIVES_ID: &str = "xcode-archives";
const ARCHIVES_LABEL: &str = "Xcode Archives";

impl CleanProvider for XcodeArchives {
    fn id(&self) -> &'static str {
        ARCHIVES_ID
    }
    fn label(&self) -> &'static str {
        ARCHIVES_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "App archives from past Xcode builds; needed only to re-export or symbolicate those builds"
            .into()
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
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let root = h.join("Library/Developer/Xcode/Archives");
        Ok(root_as_item(
            ctx,
            &root,
            ARCHIVES_ID,
            ARCHIVES_LABEL,
            RiskLevel::Review,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ARCHIVES_ID)
    }
}

pub struct XcodeDeviceSupport;

const DS_ID: &str = "xcode-devicesupport";
const DS_LABEL: &str = "Xcode iOS DeviceSupport";

impl CleanProvider for XcodeDeviceSupport {
    fn id(&self) -> &'static str {
        DS_ID
    }
    fn label(&self) -> &'static str {
        DS_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Device support files Xcode re-downloads when a device connects".into()
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
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let root = h.join("Library/Developer/Xcode/iOS DeviceSupport");
        Ok(root_as_item(ctx, &root, DS_ID, DS_LABEL, RiskLevel::Review))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, DS_ID)
    }
}

pub struct XcodePreviews;

const PREVIEWS_ID: &str = "xcode-previews";
const PREVIEWS_LABEL: &str = "Xcode Previews";
/// Previews' build products and the simulator devices that render them.
const PREVIEWS_DIR: &str = "Library/Developer/Xcode/UserData/Previews";

impl CleanProvider for XcodePreviews {
    fn id(&self) -> &'static str {
        PREVIEWS_ID
    }
    fn label(&self) -> &'static str {
        PREVIEWS_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "SwiftUI preview builds and their simulators; Xcode makes them again the next time a \
         preview runs"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    /// Xcode runs the previews, so it holds them open while it runs.
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(APP)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let Some(h) = home() else {
            return Ok(Vec::new());
        };
        Ok(previews(ctx, &h))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, PREVIEWS_ID)
    }
}

/// `home`'s Previews folder as one item, when it is a real folder.
fn previews(ctx: &ScanContext<'_>, home: &std::path::Path) -> Vec<CleanItem> {
    let root = home.join(PREVIEWS_DIR);
    if !crate::clean::fs_safe::is_dir_safe(&root) {
        return Vec::new();
    }
    root_as_item(ctx, &root, PREVIEWS_ID, PREVIEWS_LABEL, RiskLevel::Safe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::discover::{discover_checked, CategoryOutcome};
    use crate::clean::process::test_support::MockChecker;

    #[test]
    fn previews_are_one_item_moved_like_any_path() {
        let home = tempfile::tempdir().unwrap();
        assert!(previews(&ScanContext::unchecked(), home.path()).is_empty());
        let root = home.path().join(PREVIEWS_DIR);
        std::fs::create_dir_all(root.join("Simulator Devices")).unwrap();
        std::fs::write(root.join("Simulator Devices/x"), vec![1u8; 8192]).unwrap();
        let items = previews(&ScanContext::unchecked(), home.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, root);
        assert!(items[0].size >= 8192);
        assert_eq!(items[0].risk, RiskLevel::Safe);
        assert!(XcodePreviews.desktop_trash_paths());
        assert_eq!(XcodePreviews.desktop_tool(), None);
    }

    #[test]
    fn previews_are_skipped_while_xcode_runs() {
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(XcodePreviews)];
        let checker = MockChecker::with_running(["Xcode"]);
        let found = discover_checked(&providers, &ScanContext::new(None, &checker), None);
        assert!(matches!(
            &found.categories[0].outcome,
            CategoryOutcome::AppRunning { app } if app == "Xcode"
        ));
        assert_eq!(
            XcodePreviews.item_apps(std::path::Path::new("/x")),
            ["Xcode"]
        );
    }
}
