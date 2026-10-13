//! iOS Simulator devices that `xcrun simctl list devices -j` reports as
//! unavailable (usually their runtime was removed). Discovery lists each
//! one's data folder; `execute` runs `xcrun simctl delete <udid>` per item so
//! CoreSimulator updates its registry. Never `delete unavailable`, never a
//! filesystem removal.
//!
//! Risk = **Destructive**: a deleted device and its data are gone. As for
//! Time Machine snapshots, Trash and HardDelete run the same command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::ios_simulators::SIMULATOR_APPS;
use super::{run_tool, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, is_dir_safe};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "simulator-devices";
const LABEL: &str = "Unavailable iOS Simulator devices";
/// Absolute: a Finder-launched app has a minimal `PATH`.
const XCRUN: &str = "/usr/bin/xcrun";
const LIST_ARGS: &[&str] = &["simctl", "list", "devices", "-j"];
const FIND_ARGS: &[&str] = &["--find", "simctl"];
const DEVICES_DIR: &str = "Library/Developer/CoreSimulator/Devices";
/// Hex digits per dash-separated group of a UDID.
const UDID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// `simctl list devices -j`: runtime id -> devices.
#[derive(serde::Deserialize)]
struct DeviceList {
    devices: BTreeMap<String, Vec<Device>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    udid: String,
    name: String,
    /// Missing in output we do not understand; such a device is not offered.
    is_available: Option<bool>,
    availability_error: Option<String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct SimulatorDevices {
    runner: Arc<dyn CommandRunner>,
}

impl SimulatorDevices {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn simctl_found(&self) -> bool {
        run_tool(self.runner.as_ref(), XCRUN, FIND_ARGS).is_ok_and(|out| out.success())
    }

    /// Unavailable devices whose folder exists under `devices_root`. The
    /// path is built from the validated UDID, never from simctl's `dataPath`.
    fn unavailable_devices(
        &self,
        ctx: &ScanContext<'_>,
        devices_root: &Path,
    ) -> Result<Vec<CleanItem>> {
        // Without the folder there is nothing to offer; skipping the spawn
        // also keeps the `xcrun` shim from asking to install developer tools.
        if !is_dir_safe(devices_root) {
            return Ok(Vec::new());
        }
        let out = run_tool(self.runner.as_ref(), XCRUN, LIST_ARGS)?;
        if !out.success() {
            return Err(engine_error!(
                "xcrun simctl list devices failed: {}",
                out.stderr.trim()
            ));
        }
        let list: DeviceList = serde_json::from_str(&out.stdout)
            .map_err(|e| engine_error!("unexpected xcrun simctl output: {e}"))?;
        ctx.add_root(devices_root);
        let mut items = Vec::new();
        for device in list.devices.into_values().flatten() {
            if device.is_available != Some(false) || !is_udid(&device.udid) {
                continue;
            }
            let path = devices_root.join(&device.udid);
            if !is_dir_safe(&path) {
                continue;
            }
            let size = dir_size_checked(&path, ctx);
            items.push(CleanItem {
                category_id: ID.to_string(),
                category_label: LABEL.to_string(),
                path,
                size,
                risk: RiskLevel::Destructive,
                evidence: vec![Evidence::SimulatorUnavailable {
                    name: device.name,
                    reason: device.availability_error.unwrap_or_default(),
                }],
            });
        }
        Ok(items)
    }
}

impl CleanProvider for SimulatorDevices {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Simulator devices xcrun simctl reports as unavailable, usually because their runtime is gone"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Destructive
    }
    fn quit_apps(&self) -> Vec<&'static str> {
        SIMULATOR_APPS.to_vec()
    }
    /// Without a Devices folder there is nothing to offer and discovery
    /// spawns nothing; with one, `simctl` must exist (a Mac with only the
    /// Command Line Tools has `xcrun` but not `simctl`).
    fn available(&self) -> bool {
        match home() {
            Some(h) if is_dir_safe(&h.join(DEVICES_DIR)) => self.simctl_found(),
            _ => true,
        }
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("simctl")
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        match home() {
            Some(h) => self.unavailable_devices(ctx, &h.join(DEVICES_DIR)),
            None => Ok(Vec::new()),
        }
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        if matches!(action, ExecAction::EmptyTrash) {
            return Err(engine_error!("{} provider does not accept EmptyTrash", ID));
        }
        let mut report = ExecReport::default();
        for item in items {
            let udid = item
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| is_udid(name));
            let Some(udid) = udid else {
                report
                    .failed
                    .push((item.path.clone(), "not a simulator device folder".into()));
                continue;
            };
            if self.runner.run(XCRUN, &["simctl", "delete", udid]).success {
                report.removed_paths.push(item.path.clone());
            } else {
                let reason = format!("xcrun simctl delete {udid} failed");
                report.failed.push((item.path.clone(), reason));
            }
        }
        Ok(report)
    }
}

/// `01DADBA3-1A21-4914-A093-29B4D0D7B9C8`: checked before a UDID becomes a
/// path segment or a `simctl delete` argument.
fn is_udid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == UDID_GROUPS.len()
        && groups
            .iter()
            .zip(UDID_GROUPS)
            .all(|(g, len)| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use crate::runner::test_support::MockRunner;

    const GONE: &str = "01DADBA3-1A21-4914-A093-29B4D0D7B9C8";
    const LIVE: &str = "6F1C0E52-7A0B-4C1D-9E2F-3A4B5C6D7E8F";
    const NO_DIR: &str = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";

    fn list_json() -> String {
        format!(
            r#"{{"devices": {{
  "com.apple.CoreSimulator.SimRuntime.iOS-17-0": [
    {{"udid": "{GONE}", "name": "iPhone 15", "isAvailable": false,
      "availabilityError": "runtime profile not found",
      "dataPath": "/elsewhere/{GONE}/data", "state": "Shutdown"}},
    {{"udid": "{NO_DIR}", "name": "iPad", "isAvailable": false}},
    {{"udid": "../../Documents", "name": "Evil", "isAvailable": false}}
  ],
  "com.apple.CoreSimulator.SimRuntime.iOS-18-0": [
    {{"udid": "{LIVE}", "name": "iPhone 16", "isAvailable": true}}
  ]
}}}}"#
        )
    }

    fn devices_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("tiny-sim-devices-{label}-{}", std::process::id()));
        for udid in [GONE, LIVE] {
            std::fs::create_dir_all(root.join(udid).join("data")).unwrap();
        }
        std::fs::write(root.join(GONE).join("data/file"), b"12345").unwrap();
        std::fs::write(root.join("device_set.plist"), b"<plist/>").unwrap();
        root
    }

    fn listing_runner() -> MockRunner {
        MockRunner::new().with_exit(XCRUN, LIST_ARGS, 0, &list_json())
    }

    fn item(path: PathBuf) -> CleanItem {
        CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path,
            size: 0,
            risk: RiskLevel::Destructive,
            evidence: Vec::new(),
        }
    }

    fn cleanup(root: &Path) {
        let _ = crate::clean::fs_safe::remove_recursive_safe(root);
    }

    #[test]
    fn only_unavailable_devices_with_a_folder_are_offered() {
        let root = devices_root("discover");
        let p = SimulatorDevices::with_runner(Arc::new(listing_runner()));
        let items = p
            .unavailable_devices(&ScanContext::unchecked(), &root)
            .unwrap();
        assert_eq!(items.len(), 1, "{:?}", items);
        assert_eq!(items[0].path, root.join(GONE));
        assert_eq!(items[0].size, 5);
        assert_eq!(items[0].risk, RiskLevel::Destructive);
        assert_eq!(
            items[0].evidence,
            vec![Evidence::SimulatorUnavailable {
                name: "iPhone 15".into(),
                reason: "runtime profile not found".into()
            }]
        );
        cleanup(&root);
    }

    #[test]
    fn a_missing_devices_folder_spawns_nothing() {
        let runner = Arc::new(listing_runner());
        let p = SimulatorDevices::with_runner(runner.clone());
        let missing = std::env::temp_dir().join("tiny-sim-devices-none-xyz");
        let items = p
            .unavailable_devices(&ScanContext::unchecked(), &missing)
            .unwrap();
        assert!(items.is_empty());
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_missing_simctl_is_reported_as_a_missing_tool() {
        let missing = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 72, "");
        assert!(!SimulatorDevices::with_runner(Arc::new(missing)).simctl_found());
        let found = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 0, "/usr/bin/simctl\n");
        let p = SimulatorDevices::with_runner(Arc::new(found));
        assert!(p.simctl_found());
        assert_eq!(p.required_tool(), Some("simctl"));
    }

    #[test]
    fn a_failed_or_unparsable_listing_is_a_discovery_error() {
        let root = devices_root("errors");
        let ctx = ScanContext::unchecked();
        let failed = MockRunner::new().with_exit(XCRUN, LIST_ARGS, 72, "");
        let p = SimulatorDevices::with_runner(Arc::new(failed));
        assert!(p.unavailable_devices(&ctx, &root).is_err());
        let garbage = MockRunner::new().with_exit(XCRUN, LIST_ARGS, 0, "== Devices ==");
        let p = SimulatorDevices::with_runner(Arc::new(garbage));
        assert!(p.unavailable_devices(&ctx, &root).is_err());
        cleanup(&root);
    }

    #[test]
    fn execute_runs_simctl_delete_per_device_and_never_touches_the_folder() {
        let root = devices_root("execute");
        let runner =
            Arc::new(MockRunner::new().with_response(XCRUN, &["simctl", "delete", GONE], true, ""));
        let p = SimulatorDevices::with_runner(runner.clone());
        for action in [ExecAction::Trash, ExecAction::HardDelete] {
            let report = p.execute(&[item(root.join(GONE))], action).unwrap();
            assert_eq!(report.removed_paths, vec![root.join(GONE)]);
            assert!(report.failed.is_empty());
            assert!(is_dir_safe(&root.join(GONE).join("data")));
        }
        let calls = runner.run_calls.lock().unwrap().clone();
        let expected = format!("{XCRUN} simctl delete {GONE}");
        assert_eq!(calls, vec![expected.clone(), expected]);
        assert!(calls.iter().all(|c| !c.contains("unavailable")));
        cleanup(&root);
    }

    #[test]
    fn execute_refuses_a_non_udid_path_and_empty_trash() {
        let runner = Arc::new(MockRunner::new());
        let p = SimulatorDevices::with_runner(runner.clone());
        let report = p
            .execute(
                &[item(PathBuf::from("/x/../../Documents"))],
                ExecAction::HardDelete,
            )
            .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(p
            .execute(&[item(PathBuf::from(GONE))], ExecAction::EmptyTrash)
            .is_err());
        assert!(runner.run_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_delete_is_reported() {
        let p = SimulatorDevices::with_runner(Arc::new(MockRunner::new()));
        let report = p
            .execute(&[item(PathBuf::from(GONE))], ExecAction::HardDelete)
            .unwrap();
        assert!(report.removed_paths.is_empty());
        assert_eq!(
            report.failed[0].1,
            format!("xcrun simctl delete {GONE} failed")
        );
    }

    #[test]
    fn udids_are_validated_strictly() {
        assert!(is_udid(GONE));
        assert!(is_udid(&GONE.to_lowercase()));
        for bad in [
            "",
            "..",
            "../../Documents",
            "01DADBA3-1A21-4914-A093",
            "01DADBA3-1A21-4914-A093-29B4D0D7B9C8-00",
            "01DADBA3_1A21_4914_A093_29B4D0D7B9C8",
            "ZZDADBA3-1A21-4914-A093-29B4D0D7B9C8",
            "unavailable",
        ] {
            assert!(!is_udid(bad), "{bad}");
        }
    }

    #[test]
    fn simulator_devices_is_registered_destructive_and_gated() {
        let p = SimulatorDevices::with_runner(Arc::new(MockRunner::new()));
        assert!(known_category_ids().contains(&ID));
        assert_eq!(p.risk(), RiskLevel::Destructive);
        assert_eq!(p.quit_apps(), vec!["Xcode", "Simulator"]);
    }
}
