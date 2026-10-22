//! Simulator runtimes no simulator device uses, when `simctl` exists.
//! `xcrun simctl runtime list -j` (keyed by image UUID) is joined with
//! `xcrun simctl list devices -j` (keyed by runtime identifier) on
//! `runtimeIdentifier`, never on display names: `runtime list` names an
//! image by its full version ("iOS 26.4.1") while devices group under the
//! short one ("iOS 26.4"), as Mole's `_simctl_orphan_runtime_rows` records.
//!
//! An image is offered only when it is Ready, deletable, the only image for
//! its runtime identifier (devices cannot say which of two they use) and no
//! device uses it. Removal is `xcrun simctl runtime delete <image UUID>`,
//! which simctl finishes in the background, so it is reported as requested.
//! Without `simctl`, `simulator-runtime-images` reports the runtimes instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::ios_simulators::SIMULATOR_APPS;
use super::simulator_devices::{is_udid, simctl_found, LIST_ARGS, XCRUN};
use super::{run_tool, CleanProvider};
use crate::clean::fs_safe::is_dir_safe;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "simulator-runtimes";
const LABEL: &str = "Unused simulator runtimes";
const RUNTIME_ARGS: &[&str] = &["simctl", "runtime", "list", "-j"];
/// System-wide CoreSimulator data; without it no runtime is installed and
/// `xcrun` is never spawned.
pub(super) const CORESIMULATOR: &str = "/Library/Developer/CoreSimulator";

const NOT_A_RUNTIME: &str = "not a simulator runtime";

const PLACEHOLDER_PREFIX: &str = "<simulator-runtime:";
const PLACEHOLDER_SUFFIX: &str = ">";

/// A runtime image has no path TinySift may act on, so its item path names
/// the image UUID: `<simulator-runtime:UUID>`.
pub(super) fn placeholder(uuid: &str) -> PathBuf {
    PathBuf::from(format!("{PLACEHOLDER_PREFIX}{uuid}{PLACEHOLDER_SUFFIX}"))
}

/// The image UUID of a placeholder, only when it is a well-formed UUID.
pub(super) fn placeholder_uuid(path: &Path) -> Option<&str> {
    path.to_str()?
        .strip_prefix(PLACEHOLDER_PREFIX)?
        .strip_suffix(PLACEHOLDER_SUFFIX)
        .filter(|uuid| is_udid(uuid))
}

/// One image of `simctl runtime list -j`. Optional fields that are missing
/// keep the image from being offered.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeImage {
    runtime_identifier: String,
    version: Option<String>,
    build: Option<String>,
    size_bytes: Option<u64>,
    deletable: Option<bool>,
    state: Option<String>,
}

/// `simctl list devices -j`: runtime identifier -> devices.
#[derive(serde::Deserialize)]
struct DeviceList {
    devices: BTreeMap<String, Vec<DeviceEntry>>,
}

#[derive(serde::Deserialize)]
struct DeviceEntry {
    #[allow(dead_code)]
    udid: String,
}

/// Images (by UUID) that are Ready, deletable, the only image for their
/// runtime identifier and used by no device.
fn orphans(
    runtimes: BTreeMap<String, RuntimeImage>,
    devices: &DeviceList,
) -> Vec<(String, RuntimeImage)> {
    let mut images_per_runtime: BTreeMap<String, usize> = BTreeMap::new();
    for image in runtimes.values() {
        *images_per_runtime
            .entry(image.runtime_identifier.clone())
            .or_default() += 1;
    }
    let unused = |image: &RuntimeImage| {
        images_per_runtime.get(&image.runtime_identifier) == Some(&1)
            && devices
                .devices
                .get(&image.runtime_identifier)
                .is_none_or(Vec::is_empty)
    };
    runtimes
        .into_iter()
        .filter(|(uuid, image)| {
            is_udid(uuid)
                && image.state.as_deref() == Some("Ready")
                && image.deletable == Some(true)
                && unused(image)
        })
        .collect()
}

pub struct SimulatorRuntimes {
    runner: Arc<dyn CommandRunner>,
    coresimulator: PathBuf,
}

impl SimulatorRuntimes {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            coresimulator: PathBuf::from(CORESIMULATOR),
        }
    }

    fn simctl_json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        let out = run_tool(self.runner.as_ref(), XCRUN, args)?;
        if !out.success() {
            return Err(engine_error!(
                "xcrun {} failed: {}",
                args.join(" "),
                out.stderr.trim()
            ));
        }
        serde_json::from_str(&out.stdout)
            .map_err(|e| engine_error!("unexpected xcrun {} output: {e}", args.join(" ")))
    }

    fn unused_runtimes(&self) -> Result<Vec<(String, RuntimeImage)>> {
        let runtimes = self.simctl_json(RUNTIME_ARGS)?;
        let devices: DeviceList = self.simctl_json(LIST_ARGS)?;
        Ok(orphans(runtimes, &devices))
    }
}

impl CleanProvider for SimulatorRuntimes {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Simulator runtimes that are ready and deletable and that no simulator device uses, per xcrun simctl"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn quit_apps(&self) -> Vec<&'static str> {
        SIMULATOR_APPS.to_vec()
    }
    /// Only with `simctl`; without it `simulator-runtime-images` reports
    /// the runtimes, so this category is left out rather than unavailable.
    fn available(&self) -> bool {
        is_dir_safe(&self.coresimulator) && simctl_found(self.runner.as_ref())
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let mut items = Vec::new();
        for (uuid, image) in self.unused_runtimes()? {
            let path = placeholder(&uuid);
            // simctl's figure, not the blocks deleting would free.
            ctx.note_size_upper_bound(&path);
            items.push(CleanItem {
                category_id: ID.to_string(),
                category_label: LABEL.to_string(),
                path,
                size: image.size_bytes.unwrap_or(0),
                risk: RiskLevel::Review,
                evidence: vec![Evidence::SimulatorRuntime {
                    identifier: image.runtime_identifier,
                    version: image.version.unwrap_or_default(),
                    build: image.build.unwrap_or_default(),
                }],
            });
        }
        Ok(items)
    }
    /// The join again, right before the delete: a device created since the
    /// scan keeps its runtime.
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        let uuid = placeholder_uuid(path).ok_or(NOT_A_RUNTIME)?;
        let unused = self
            .unused_runtimes()
            .map_err(|e| format!("could not check simulator runtimes again: {e}"))?;
        if unused.iter().any(|(image, _)| image == uuid) {
            Ok(())
        } else {
            Err("no longer an unused simulator runtime".into())
        }
    }
    /// Trash and HardDelete both run `simctl runtime delete`: a runtime
    /// has no Trash semantics.
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        if matches!(action, ExecAction::EmptyTrash) {
            return Err(engine_error!("{} provider does not accept EmptyTrash", ID));
        }
        let mut report = ExecReport::default();
        for item in items {
            let Some(uuid) = placeholder_uuid(&item.path) else {
                report
                    .failed
                    .push((item.path.clone(), NOT_A_RUNTIME.to_string()));
                continue;
            };
            let delete = ["simctl", "runtime", "delete", uuid];
            if self.runner.run(XCRUN, &delete).success {
                report.removal_requested.push(item.path.clone());
            } else {
                let reason = format!("xcrun simctl runtime delete {uuid} failed");
                report.failed.push((item.path.clone(), reason));
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::simulator_devices::FIND_ARGS;
    use crate::clean::providers::{desktop_report_only, known_category_ids, ReportOnly};
    use crate::runner::test_support::MockRunner;

    const ORPHAN: &str = "0A8C5D2E-1111-4A1B-8C2D-3E4F5A6B7C8D";
    const WITH_DEVICES: &str = "0A8C5D2E-2222-4A1B-8C2D-3E4F5A6B7C8D";
    const TWIN_A: &str = "0A8C5D2E-3333-4A1B-8C2D-3E4F5A6B7C8D";
    const TWIN_B: &str = "0A8C5D2E-4444-4A1B-8C2D-3E4F5A6B7C8D";
    const NOT_DELETABLE: &str = "0A8C5D2E-5555-4A1B-8C2D-3E4F5A6B7C8D";
    const DELETING: &str = "0A8C5D2E-6666-4A1B-8C2D-3E4F5A6B7C8D";
    const POINT_RELEASE: &str = "0A8C5D2E-7777-4A1B-8C2D-3E4F5A6B7C8D";
    const RT: &str = "com.apple.CoreSimulator.SimRuntime";

    fn image(uuid: &str, runtime: &str, version: &str, deletable: bool, state: &str) -> String {
        format!(
            r#""{uuid}": {{
    "build": "B-{version}", "deletable": {deletable}, "identifier": "{uuid}",
    "kind": "Disk Image", "path": "/System/Library/AssetsV2/x/{uuid}.dmg",
    "platformIdentifier": "com.apple.platform.iphonesimulator",
    "runtimeIdentifier": "{RT}.{runtime}", "signatureState": "Verified",
    "sizeBytes": 8000000000, "state": "{state}", "version": "{version}"
  }}"#
        )
    }

    fn runtime_json() -> String {
        let images = [
            image(ORPHAN, "iOS-17-0", "17.0", true, "Ready"),
            image(WITH_DEVICES, "iOS-18-0", "18.0", true, "Ready"),
            image(TWIN_A, "watchOS-10-0", "10.0", true, "Ready"),
            image(TWIN_B, "watchOS-10-0", "10.0.1", true, "Ready"),
            image(NOT_DELETABLE, "tvOS-17-0", "17.0", false, "Ready"),
            image(DELETING, "xrOS-1-0", "1.0", true, "Deleting"),
            // Devices group under "iOS 26.4"; a name join would miss them.
            image(POINT_RELEASE, "iOS-26-4", "26.4.1", true, "Ready"),
            image("../../Documents", "iOS-15-0", "15.0", true, "Ready"),
        ];
        format!("{{{}}}", images.join(",\n"))
    }

    fn devices_json() -> String {
        format!(
            r#"{{"devices": {{
  "{RT}.iOS-17-0": [],
  "{RT}.iOS-18-0": [{{"udid": "6F1C0E52-7A0B-4C1D-9E2F-3A4B5C6D7E8F", "name": "iPhone 16", "isAvailable": true}}],
  "{RT}.iOS-26-4": [{{"udid": "6F1C0E52-7A0B-4C1D-9E2F-3A4B5C6D7E90", "name": "iPhone 17", "isAvailable": true}}]
}}}}"#
        )
    }

    fn listing() -> MockRunner {
        MockRunner::new()
            .with_exit(XCRUN, RUNTIME_ARGS, 0, &runtime_json())
            .with_exit(XCRUN, LIST_ARGS, 0, &devices_json())
    }

    fn provider(runner: MockRunner) -> (SimulatorRuntimes, Arc<MockRunner>) {
        let runner = Arc::new(runner);
        (SimulatorRuntimes::with_runner(runner.clone()), runner)
    }

    fn item(path: PathBuf) -> CleanItem {
        CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path,
            size: 0,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn only_unused_ready_deletable_sole_images_are_offered() {
        let (p, _) = provider(listing());
        let ctx = ScanContext::unchecked();
        let items = p.discover(&ctx).unwrap();
        let paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(paths, vec![placeholder(ORPHAN)]);
        assert_eq!(items[0].size, 8_000_000_000);
        assert_eq!(items[0].risk, RiskLevel::Review);
        assert_eq!(
            items[0].evidence,
            vec![Evidence::SimulatorRuntime {
                identifier: format!("{RT}.iOS-17-0"),
                version: "17.0".into(),
                build: "B-17.0".into(),
            }]
        );
        assert!(ctx
            .take_findings()
            .size_upper_bound
            .contains(&placeholder(ORPHAN)));
    }

    #[test]
    fn devices_are_matched_on_the_runtime_identifier_not_the_version_name() {
        let runtimes: BTreeMap<String, RuntimeImage> =
            serde_json::from_str(&runtime_json()).unwrap();
        let devices: DeviceList = serde_json::from_str(&devices_json()).unwrap();
        let unused: Vec<String> = orphans(runtimes, &devices)
            .into_iter()
            .map(|(uuid, _)| uuid)
            .collect();
        assert!(!unused.contains(&POINT_RELEASE.to_string()));
        assert!(!unused.contains(&WITH_DEVICES.to_string()));
        assert!(!unused.contains(&TWIN_A.to_string()), "two images, one id");
        assert!(!unused.contains(&TWIN_B.to_string()));
        assert!(!unused.contains(&NOT_DELETABLE.to_string()));
        assert!(!unused.contains(&DELETING.to_string()));
        assert_eq!(unused, vec![ORPHAN.to_string()]);
    }

    #[test]
    fn a_failed_or_unparsable_listing_fails_discovery() {
        let ctx = ScanContext::unchecked();
        let runners = [
            MockRunner::new()
                .with_exit(XCRUN, RUNTIME_ARGS, 1, "")
                .with_exit(XCRUN, LIST_ARGS, 0, &devices_json()),
            MockRunner::new()
                .with_exit(XCRUN, RUNTIME_ARGS, 0, "== Runtimes ==")
                .with_exit(XCRUN, LIST_ARGS, 0, &devices_json()),
            MockRunner::new()
                .with_exit(XCRUN, RUNTIME_ARGS, 0, &runtime_json())
                .with_exit(XCRUN, LIST_ARGS, 1, ""),
            MockRunner::new()
                .with_exit(XCRUN, RUNTIME_ARGS, 0, &runtime_json())
                .with_exit(
                    XCRUN,
                    LIST_ARGS,
                    0,
                    r#"{"devices": {"x": [{"name": "no udid"}]}}"#,
                ),
        ];
        for runner in runners {
            assert!(provider(runner).0.discover(&ctx).is_err());
        }
    }

    #[test]
    fn available_only_with_simctl_and_never_spawns_without_coresimulator() {
        let dir = tempfile::tempdir().unwrap();
        let with = |runner: MockRunner, root: PathBuf| {
            let runner = Arc::new(runner);
            let p = SimulatorRuntimes {
                runner: runner.clone(),
                coresimulator: root,
            };
            (p.available(), runner)
        };
        let (found, runner) = with(listing(), dir.path().join("missing"));
        assert!(!found);
        assert!(runner.output_calls.lock().unwrap().is_empty());
        let no_simctl = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 72, "");
        assert!(!with(no_simctl, dir.path().to_path_buf()).0);
        let simctl = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 0, "/usr/bin/simctl\n");
        assert!(with(simctl, dir.path().to_path_buf()).0);
        let (p, _) = provider(MockRunner::new());
        assert_eq!(p.required_tool(), None, "left out, not unavailable");
    }

    #[test]
    fn execute_requests_removal_through_simctl_per_image() {
        let delete = ["simctl", "runtime", "delete", ORPHAN];
        let (p, runner) = provider(MockRunner::new().with_response(XCRUN, &delete, true, ""));
        for action in [ExecAction::Trash, ExecAction::HardDelete] {
            let report = p.execute(&[item(placeholder(ORPHAN))], action).unwrap();
            assert_eq!(report.removal_requested, vec![placeholder(ORPHAN)]);
            assert!(report.removed_paths.is_empty(), "not freed yet");
            assert!(report.failed.is_empty());
        }
        let expected = format!("{XCRUN} simctl runtime delete {ORPHAN}");
        assert_eq!(
            runner.run_calls.lock().unwrap().clone(),
            vec![expected.clone(), expected]
        );
    }

    #[test]
    fn a_failed_delete_is_reported() {
        let (p, _) = provider(MockRunner::new());
        let report = p
            .execute(&[item(placeholder(ORPHAN))], ExecAction::HardDelete)
            .unwrap();
        assert!(report.removal_requested.is_empty());
        assert_eq!(
            report.failed,
            vec![(
                placeholder(ORPHAN),
                format!("xcrun simctl runtime delete {ORPHAN} failed")
            )]
        );
    }

    #[test]
    fn execute_refuses_anything_but_a_runtime_placeholder() {
        let (p, runner) = provider(MockRunner::new());
        let paths = [
            PathBuf::from("/System/Library/AssetsV2/x/0A8C5D2E-1111-4A1B-8C2D-3E4F5A6B7C8D.dmg"),
            PathBuf::from("<simulator-runtime:../../Documents>"),
            PathBuf::from(format!("/tmp/{ORPHAN}")),
        ];
        let items: Vec<_> = paths.iter().cloned().map(item).collect();
        let report = p.execute(&items, ExecAction::HardDelete).unwrap();
        assert_eq!(report.failed.len(), paths.len());
        assert!(report.removal_requested.is_empty());
        assert!(runner.run_calls.lock().unwrap().is_empty());
        assert!(p.execute(&items, ExecAction::EmptyTrash).is_err());
    }

    #[test]
    fn the_guard_repeats_the_join_before_each_delete() {
        let (p, _) = provider(listing());
        assert!(p.check_item(&placeholder(ORPHAN)).is_ok());
        assert!(p.check_item(&placeholder(WITH_DEVICES)).is_err());
        assert!(p.check_item(Path::new("/System/Library/AssetsV2")).is_err());
        let (failing, _) = provider(MockRunner::new());
        assert!(failing.check_item(&placeholder(ORPHAN)).is_err());
    }

    #[test]
    fn placeholders_carry_only_well_formed_uuids() {
        assert_eq!(placeholder_uuid(&placeholder(ORPHAN)), Some(ORPHAN));
        for bad in [
            "<simulator-runtime:../../x>",
            "<simulator-runtime:>",
            "/System/Library/AssetsV2",
            "<tmutil:com.apple.TimeMachine.2026-10-22-120000.local>",
        ] {
            assert_eq!(placeholder_uuid(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn registered_review_and_report_only_on_the_desktop() {
        let (p, _) = provider(MockRunner::new());
        assert!(known_category_ids().contains(&ID));
        assert_eq!(p.risk(), RiskLevel::Review);
        assert!(!p.needs_admin(), "simctl runtime delete runs without sudo");
        assert_eq!(desktop_report_only(&p), Some(ReportOnly::NotPerPathTrash));
        assert_eq!(p.quit_apps(), vec!["Xcode", "Simulator"]);
    }
}
