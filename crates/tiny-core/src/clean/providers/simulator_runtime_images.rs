//! Simulator runtimes when `simctl` is missing (Xcode is not installed):
//! read from CoreSimulator's image registry and only reported, because then
//! only System Settings > General > Storage removes them. Each runtime's
//! disk image lies under `/System/Library/AssetsV2`, which TinySift never
//! changes; it is only sized there, by its allocated blocks.
//!
//! `plutil -extract` reads one key path at a time: `plutil -convert json`
//! rejects the registry, whose `<date>` and `<data>` values JSON cannot hold.

use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::simulator_devices::{is_udid, simctl_found};
use super::simulator_runtimes::placeholder;
use super::{run_tool, CleanProvider, ReportOnly};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "simulator-runtime-images";
const LABEL: &str = "Simulator runtimes";
/// Absolute: a Finder-launched app has a minimal `PATH`.
const PLUTIL: &str = "/usr/bin/plutil";
const REGISTRY: &str = "/Library/Developer/CoreSimulator/Images/images.plist";
/// The only folder whose disk images are sized.
const ASSETS: &str = "/System/Library/AssetsV2";
/// Bounds the `plutil` calls one scan makes.
const MAX_IMAGES: usize = 64;
const REPORT_ONLY: &str =
    "report only: Xcode is not installed; remove simulator runtimes in System Settings > General > Storage";

/// `runtimeInfo` of one registry image.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeInfo {
    bundle_identifier: String,
    /// Packed as `major << 16 | minor << 8 | patch`.
    version: Option<u64>,
    build: Option<String>,
}

pub struct SimulatorRuntimeImages {
    runner: Arc<dyn CommandRunner>,
    registry: PathBuf,
    assets: PathBuf,
}

impl SimulatorRuntimeImages {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            registry: PathBuf::from(REGISTRY),
            assets: PathBuf::from(ASSETS),
        }
    }

    fn registry_exists(&self) -> bool {
        std::fs::symlink_metadata(&self.registry).is_ok_and(|m| m.file_type().is_file())
    }

    /// One registry value: `plutil -extract <keypath> <format> -expect <kind>`.
    fn extract(&self, keypath: &str, format: &str, kind: &str) -> Result<String> {
        let registry = self
            .registry
            .to_str()
            .ok_or_else(|| engine_error!("the image registry path is not UTF-8"))?;
        let args = [
            "-extract", keypath, format, "-expect", kind, "-o", "-", registry,
        ];
        let out = run_tool(self.runner.as_ref(), PLUTIL, &args)?;
        if !out.success() {
            return Err(engine_error!(
                "plutil could not read {keypath}: {}",
                out.stderr.trim()
            ));
        }
        Ok(out.stdout.trim_end_matches('\n').to_string())
    }

    /// Every registered runtime whose image is under `assets`; images
    /// pointing elsewhere are refused, missing ones skipped.
    fn runtimes(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let count: usize = self
            .extract("images", "raw", "array")?
            .parse()
            .map_err(|e| engine_error!("unexpected image count in the registry: {e}"))?;
        let mut items = Vec::new();
        for index in 0..count.min(MAX_IMAGES) {
            let uuid = self.extract(&format!("images.{index}.uuid"), "raw", "string")?;
            if !is_udid(&uuid) {
                return Err(engine_error!("unexpected image id in the registry: {uuid}"));
            }
            let path = placeholder(&uuid);
            match self.runtime(index, path.clone()) {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(reason) => ctx.note_refused(&path, reason),
            }
        }
        Ok(items)
    }

    /// Image `index` as an item at `path`, `None` when its disk image is
    /// gone, `Err` with why it cannot be sized.
    fn runtime(
        &self,
        index: usize,
        path: PathBuf,
    ) -> std::result::Result<Option<CleanItem>, String> {
        let info = self
            .extract(&format!("images.{index}.runtimeInfo"), "json", "dictionary")
            .map_err(|e| e.to_string())?;
        let info: RuntimeInfo =
            serde_json::from_str(&info).map_err(|e| format!("unexpected runtime info: {e}"))?;
        let url = self
            .extract(&format!("images.{index}.path.relative"), "raw", "string")
            .map_err(|e| e.to_string())?;
        let image = file_url_path(&url)
            .filter(|image| {
                !image.components().any(|c| c == Component::ParentDir)
                    && image.starts_with(&self.assets)
            })
            .ok_or_else(|| format!("its disk image is not in {}: {url}", self.assets.display()))?;
        let meta = match std::fs::symlink_metadata(&image) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", image.display())),
        };
        if !meta.file_type().is_file() {
            return Err(format!("{} is not a file", image.display()));
        }
        Ok(Some(CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path,
            size: meta.blocks().saturating_mul(512),
            risk: RiskLevel::Review,
            evidence: vec![Evidence::SimulatorRuntime {
                identifier: info.bundle_identifier,
                version: info.version.map(version_name).unwrap_or_default(),
                build: info.build.unwrap_or_default(),
            }],
        }))
    }
}

impl CleanProvider for SimulatorRuntimeImages {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Simulator runtimes CoreSimulator registered; without Xcode only System Settings can remove them"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    /// Only without `simctl`, so runtimes are never listed twice;
    /// `simulator-runtimes` covers them otherwise.
    fn available(&self) -> bool {
        self.registry_exists() && !simctl_found(self.runner.as_ref())
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        self.runtimes(ctx)
    }
    fn desktop_report_only_reason(&self) -> ReportOnly {
        ReportOnly::XcodeNotInstalled
    }
    fn check_item(&self, _path: &Path) -> std::result::Result<(), String> {
        Err(REPORT_ONLY.into())
    }
    /// Refuses every item: only System Settings removes these runtimes.
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        if matches!(action, ExecAction::EmptyTrash) {
            return Err(engine_error!("{} provider does not accept EmptyTrash", ID));
        }
        Ok(ExecReport {
            failed: items
                .iter()
                .map(|item| (item.path.clone(), REPORT_ONLY.to_string()))
                .collect(),
            ..ExecReport::default()
        })
    }
}

/// `26.0`, or `26.0.1` with a patch level.
fn version_name(packed: u64) -> String {
    let (major, minor, patch) = (packed >> 16, (packed >> 8) & 0xff, packed & 0xff);
    if patch == 0 {
        format!("{major}.{minor}")
    } else {
        format!("{major}.{minor}.{patch}")
    }
}

/// The path a `file://` URL names, percent-decoded. `None` for another
/// scheme, a bad escape, or a name that is not UTF-8 or holds NUL.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let encoded = url.strip_prefix("file://")?;
    if !encoded.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    (!path.contains('\0')).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::simulator_devices::{FIND_ARGS, XCRUN};
    use crate::clean::providers::{desktop_report_only, known_category_ids};
    use crate::runner::test_support::MockRunner;

    const TVOS: &str = "1732E568-CE24-44BA-B87A-12934CCCC76F";
    const IOS: &str = "2732E568-CE24-44BA-B87A-12934CCCC76F";
    const ELSEWHERE: &str = "3732E568-CE24-44BA-B87A-12934CCCC76F";
    const MISSING: &str = "4732E568-CE24-44BA-B87A-12934CCCC76F";
    const DOT_DOT: &str = "5732E568-CE24-44BA-B87A-12934CCCC76F";

    struct Fixture {
        _dir: tempfile::TempDir,
        registry: PathBuf,
        assets: PathBuf,
    }

    /// A registry file and an assets folder with two disk images, one in a
    /// folder whose name needs percent-encoding.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let registry = base.join("images.plist");
        std::fs::write(&registry, b"<plist/>").unwrap();
        let assets = base.join("AssetsV2");
        for (folder, bytes) in [("tvOS/Restore", 5000), ("iOS Runtime/Restore", 9000)] {
            std::fs::create_dir_all(assets.join(folder)).unwrap();
            std::fs::write(assets.join(folder).join("043.dmg"), vec![1u8; bytes]).unwrap();
        }
        Fixture {
            _dir: dir,
            registry,
            assets,
        }
    }

    fn args<'a>(keypath: &'a str, format: &'a str, kind: &'a str, file: &'a str) -> [&'a str; 8] {
        [
            "-extract", keypath, format, "-expect", kind, "-o", "-", file,
        ]
    }

    /// `plutil` answers for the registry: per image its UUID, runtime info
    /// (identifier, packed version) and disk image URL.
    fn registry_runner(f: &Fixture, images: &[(&str, &str, u64, String)]) -> MockRunner {
        let file = f.registry.to_str().unwrap();
        let count = images.len().to_string() + "\n";
        let mut runner = MockRunner::new()
            .with_exit(XCRUN, FIND_ARGS, 72, "")
            .with_exit(PLUTIL, &args("images", "raw", "array", file), 0, &count);
        for (i, (uuid, runtime, version, url)) in images.iter().enumerate() {
            let info = format!(
                r#"{{"build":"23J{i}","bundleIdentifier":"com.apple.CoreSimulator.SimRuntime.{runtime}","version":{version},"isInternal":false}}"#
            );
            runner = runner
                .with_exit(
                    PLUTIL,
                    &args(&format!("images.{i}.uuid"), "raw", "string", file),
                    0,
                    &format!("{uuid}\n"),
                )
                .with_exit(
                    PLUTIL,
                    &args(
                        &format!("images.{i}.runtimeInfo"),
                        "json",
                        "dictionary",
                        file,
                    ),
                    0,
                    &info,
                )
                .with_exit(
                    PLUTIL,
                    &args(&format!("images.{i}.path.relative"), "raw", "string", file),
                    0,
                    &format!("{url}\n"),
                );
        }
        runner
    }

    fn provider(f: &Fixture, runner: MockRunner) -> (SimulatorRuntimeImages, Arc<MockRunner>) {
        let runner = Arc::new(runner);
        let p = SimulatorRuntimeImages {
            runner: runner.clone(),
            registry: f.registry.clone(),
            assets: f.assets.clone(),
        };
        (p, runner)
    }

    fn url(path: &Path) -> String {
        format!("file://{}", path.display()).replace(' ', "%20")
    }

    fn allocated(path: &Path) -> u64 {
        std::fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    fn item(path: &str) -> CleanItem {
        CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: path.into(),
            size: 0,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn reports_each_registered_runtime_with_its_image_size() {
        let f = fixture();
        let tvos = f.assets.join("tvOS/Restore/043.dmg");
        let ios = f.assets.join("iOS Runtime/Restore/043.dmg");
        let runner = registry_runner(
            &f,
            &[
                (TVOS, "tvOS-26-0", 1_703_936, url(&tvos)),
                (IOS, "iOS-26-0", 1_703_937, url(&ios)),
            ],
        );
        let (p, runner) = provider(&f, runner);
        let ctx = ScanContext::unchecked();
        let items = p.discover(&ctx).unwrap();
        let paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(paths, vec![placeholder(TVOS), placeholder(IOS)]);
        assert_eq!(items[0].size, allocated(&tvos));
        assert_eq!(items[1].size, allocated(&ios));
        assert!(items.iter().all(|i| i.risk == RiskLevel::Review));
        assert_eq!(
            items[1].evidence,
            vec![Evidence::SimulatorRuntime {
                identifier: "com.apple.CoreSimulator.SimRuntime.iOS-26-0".into(),
                version: "26.0.1".into(),
                build: "23J1".into(),
            }]
        );
        assert!(ctx.take_findings().refused.is_empty());
        assert!(runner.run_calls.lock().unwrap().is_empty());
        assert!(runner
            .output_calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.starts_with(PLUTIL) || c.starts_with(XCRUN)));
    }

    #[test]
    fn images_outside_the_assets_folder_are_refused_and_missing_ones_skipped() {
        let f = fixture();
        let outside = f.registry.with_file_name("Images/X.dmg");
        let dot_dot = f.assets.join("tvOS/../../images.plist");
        let missing = f.assets.join("gone/043.dmg");
        let runner = registry_runner(
            &f,
            &[
                (ELSEWHERE, "iOS-17-0", 0x110000, url(&outside)),
                (DOT_DOT, "iOS-16-0", 0x100000, url(&dot_dot)),
                (MISSING, "iOS-15-0", 0x0f0000, url(&missing)),
                (
                    TVOS,
                    "tvOS-26-0",
                    1_703_936,
                    "https://example.com/x.dmg".into(),
                ),
            ],
        );
        let (p, _) = provider(&f, runner);
        let ctx = ScanContext::unchecked();
        assert!(p.discover(&ctx).unwrap().is_empty());
        let refused = ctx.take_findings().refused;
        let refused: Vec<_> = refused.keys().cloned().collect();
        assert_eq!(
            refused,
            vec![
                placeholder(TVOS),
                placeholder(ELSEWHERE),
                placeholder(DOT_DOT)
            ]
        );
    }

    #[test]
    fn a_registry_plutil_cannot_read_fails_discovery() {
        let f = fixture();
        let file = f.registry.to_str().unwrap().to_string();
        let unreadable =
            MockRunner::new().with_exit(PLUTIL, &args("images", "raw", "array", &file), 1, "");
        assert!(provider(&f, unreadable)
            .0
            .discover(&ScanContext::unchecked())
            .is_err());
        let bad_uuid = registry_runner(&f, &[("../x", "iOS-26-0", 1, url(&f.assets))]);
        assert!(provider(&f, bad_uuid)
            .0
            .discover(&ScanContext::unchecked())
            .is_err());
    }

    #[test]
    fn available_only_without_simctl_and_never_spawns_without_a_registry() {
        let f = fixture();
        let simctl = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 0, "/usr/bin/simctl\n");
        assert!(
            !provider(&f, simctl).0.available(),
            "simulator-runtimes covers it"
        );
        let no_simctl = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 72, "");
        assert!(provider(&f, no_simctl).0.available());
        let (mut p, runner) = provider(&f, MockRunner::new());
        p.registry = f.assets.join("missing.plist");
        assert!(!p.available());
        assert!(runner.output_calls.lock().unwrap().is_empty());
        assert_eq!(p.required_tool(), None);
    }

    #[test]
    fn packed_versions_unpack_to_major_minor_and_patch() {
        assert_eq!(version_name(1_703_936), "26.0");
        assert_eq!(version_name(1_703_937), "26.0.1");
        assert_eq!(version_name(0x11_02_00), "17.2");
    }

    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(
            file_url_path("file:///System/Library/AssetsV2/a%20b/043.dmg"),
            Some(PathBuf::from("/System/Library/AssetsV2/a b/043.dmg"))
        );
        for bad in [
            "https://example.com/x.dmg",
            "file:///a/%zz.dmg",
            "file:///a/%00.dmg",
            "file:///a/%ff.dmg",
            "file://host/a.dmg",
        ] {
            assert_eq!(file_url_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn nothing_is_ever_acted_on() {
        let f = fixture();
        let (p, runner) = provider(&f, MockRunner::new());
        let items = [
            item("<simulator-runtime:1732E568-CE24-44BA-B87A-12934CCCC76F>"),
            item("/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/x.asset/AssetData/Restore/043.dmg"),
        ];
        for action in [ExecAction::Trash, ExecAction::HardDelete] {
            let report = p.execute(&items, action).unwrap();
            assert!(report.removed_paths.is_empty());
            assert!(report.removal_requested.is_empty());
            assert_eq!(report.failed.len(), items.len());
            assert!(report.failed.iter().all(|(_, why)| why == REPORT_ONLY));
        }
        assert!(p.execute(&items, ExecAction::EmptyTrash).is_err());
        for item in &items {
            assert_eq!(p.check_item(&item.path), Err(REPORT_ONLY.to_string()));
        }
        assert!(runner.run_calls.lock().unwrap().is_empty());
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn registered_review_and_report_only_until_xcode_returns() {
        let p = SimulatorRuntimeImages::with_runner(Arc::new(MockRunner::new()));
        assert!(known_category_ids().contains(&ID));
        assert_eq!(p.risk(), RiskLevel::Review);
        assert_eq!(desktop_report_only(&p), Some(ReportOnly::XcodeNotInstalled));
        assert!(!p.desktop_trash_paths());
        assert!(p.quit_apps().is_empty());
    }
}
