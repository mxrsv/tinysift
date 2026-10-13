use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{execute_per_item, top_level_entries, CleanProvider};
use crate::clean::fs_safe::is_dir_safe;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "ios-simulators";
const LABEL: &str = "iOS Simulator caches";

/// Apps that use CoreSimulator data; both simulator categories wait for
/// them to quit.
pub(super) const SIMULATOR_APPS: &[&str] = &["Xcode", "Simulator"];

/// Devices are not listed here: removing one is `xcrun simctl delete`
/// (`simulator-devices`), and `Devices/` also holds `device_set.plist`,
/// the registry CoreSimulator reads to list devices.
const CACHES_DIR: &str = "Library/Developer/CoreSimulator/Caches";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct IosSimulators;

impl CleanProvider for IosSimulators {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Simulator caches in CoreSimulator/Caches that Xcode recreates".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn quit_apps(&self) -> Vec<&'static str> {
        SIMULATOR_APPS.to_vec()
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&h.join(CACHES_DIR)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| simulator_caches(ctx, &h))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Folders directly inside `home`'s `CoreSimulator/Caches`.
fn simulator_caches(ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
    top_level_entries(ctx, &home.join(CACHES_DIR), ID, LABEL, RiskLevel::Review)
        .into_iter()
        .filter(|item| is_dir_safe(&item.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;

    #[test]
    fn only_caches_are_candidates_never_devices() {
        let home = std::env::temp_dir().join(format!("tiny-sim-{}", std::process::id()));
        let sim = home.join("Library/Developer/CoreSimulator");
        let device = sim.join("Devices/01DADBA3-1A21-4914-A093-29B4D0D7B9C8");
        let cache = sim.join("Caches/dyld");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(sim.join("Devices/device_set.plist"), b"<plist/>").unwrap();
        let items = simulator_caches(&ScanContext::unchecked(), &home);
        let paths: Vec<_> = items.into_iter().map(|i| i.path).collect();
        assert_eq!(paths, vec![cache]);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&home);
    }

    #[test]
    fn ios_simulators_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn ios_simulators_gates_xcode_and_simulator() {
        let p = IosSimulators;
        assert_eq!(p.quit_apps(), vec!["Xcode", "Simulator"]);
        assert_eq!(p.item_apps(Path::new("/x")), vec!["Xcode", "Simulator"]);
    }
}
