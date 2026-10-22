//! Simulator data on the desktop (spec OC8, OC11, OC13): system caches carry
//! the administrator marker and runtimes the reason only System Settings can
//! remove them; the app acts on neither. Temp dirs and fake runners only.

use std::sync::Arc;

use tiny_core::clean::providers::system_simulator_caches::SystemSimulatorCaches;
use tiny_core::clean::providers::ReportOnly;
use tiny_core::clean::types::{Evidence, ExecAction, ExecReport};
use tiny_core::error::Result as CoreResult;
use tiny_core::runner::{CommandError, CommandOutcome, CommandOutput};

use super::*;

/// `xcrun` without `simctl`, as on a Mac with only the Command Line Tools.
struct NoSimctl;

impl CommandRunner for NoSimctl {
    fn which(&self, bin: &str) -> bool {
        panic!("{bin} must not be looked up")
    }
    fn run(&self, bin: &str, _: &[&str]) -> CommandOutput {
        panic!("{bin} must not run")
    }
    fn output(
        &self,
        bin: &str,
        args: &[&str],
        _: Duration,
    ) -> Result<CommandOutcome, CommandError> {
        assert_eq!((bin, args), ("/usr/bin/xcrun", &["--find", "simctl"][..]));
        Ok(CommandOutcome {
            status: Some(72),
            stdout: String::new(),
            stderr: "xcrun: error: unable to find utility \"simctl\"".into(),
        })
    }
}

/// One registered runtime, reported as `simulator-runtime-images` does.
struct RegisteredRuntime;

const RUNTIME: &str = "<simulator-runtime:1732E568-CE24-44BA-B87A-12934CCCC76F>";

impl CleanProvider for RegisteredRuntime {
    fn id(&self) -> &'static str {
        "simulator-runtime-images"
    }
    fn label(&self) -> &'static str {
        "Simulator runtimes"
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn discover(&self, _: &ScanContext<'_>) -> CoreResult<Vec<CleanItem>> {
        Ok(vec![CleanItem {
            category_id: self.id().into(),
            category_label: self.label().into(),
            path: PathBuf::from(RUNTIME),
            size: 3_581_926_720,
            risk: RiskLevel::Review,
            evidence: vec![Evidence::SimulatorRuntime {
                identifier: "com.apple.CoreSimulator.SimRuntime.tvOS-26-0".into(),
                version: "26.0".into(),
                build: "23J352".into(),
            }],
        }])
    }
    fn desktop_report_only_reason(&self) -> ReportOnly {
        ReportOnly::XcodeNotInstalled
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> CoreResult<ExecReport> {
        panic!("the desktop path must never call provider.execute");
    }
}

#[test]
fn system_simulator_caches_carry_the_admin_marker_and_are_report_only() {
    let dir = fixture_dir("system-simulator-caches", &["log"], 1);
    std::fs::create_dir_all(dir.join("Caches/dyld")).unwrap();
    std::fs::write(dir.join("Caches/dyld/cache"), vec![1u8; 9000]).unwrap();
    let providers: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(SystemSimulatorCaches::with_root(
            dir.join("Caches"),
            Arc::new(NoSimctl),
        )),
        Box::new(Fixture::new("logs", &dir)),
    ];
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    let caches = &discovery.categories[0];
    assert_eq!(caches.id, "system-simulator-caches");
    assert_eq!(caches.status, FfiCategoryStatus::Found);
    assert!(caches.needs_admin);
    assert_eq!(
        caches.desktop_action,
        FfiDesktopAction::ReportOnly {
            reason: FfiReportOnlyReason::NeedsAdministrator
        }
    );
    assert_eq!(caches.risk, FfiRisk::Safe);
    assert_eq!(caches.section, Some(FfiCleanSection::Rebuilt));
    assert_eq!(caches.comes_back, Some(FfiComesBack::AppRecreates));
    assert_eq!(caches.family.as_deref(), Some("dev"));
    let paths: Vec<_> = caches.candidates.iter().map(|c| c.path.clone()).collect();
    assert_eq!(paths, vec![dir.join("Caches/dyld").to_string_lossy()]);
    assert_eq!(
        caches.candidates[0].size_bytes,
        allocated(&dir.join("Caches/dyld/cache"))
    );
    assert!(!discovery.categories[1].needs_admin);
    let id = caches.candidates[0].id.clone();
    assert!(matches!(
        session.clean_preview(vec![id]),
        Err(FfiError::InvalidInput { .. })
    ));
    remove(&dir);
}

#[test]
fn registry_runtimes_say_only_system_settings_removes_them() {
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(RegisteredRuntime)];
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    let runtimes = &discovery.categories[0];
    assert_eq!(
        runtimes.desktop_action,
        FfiDesktopAction::ReportOnly {
            reason: FfiReportOnlyReason::XcodeNotInstalled
        }
    );
    assert!(!runtimes.needs_admin);
    assert_eq!(runtimes.comes_back, Some(FfiComesBack::Redownload));
    let candidate = &runtimes.candidates[0];
    assert_eq!(candidate.path, RUNTIME);
    assert_eq!(candidate.size_bytes, 3_581_926_720);
    assert_eq!(
        candidate.evidence,
        vec![FfiEvidence::SimulatorRuntime {
            identifier: "com.apple.CoreSimulator.SimRuntime.tvOS-26-0".into(),
            version: "26.0".into(),
            build: "23J352".into(),
        }]
    );
    assert!(matches!(
        session.clean_preview(vec![candidate.id.clone()]),
        Err(FfiError::InvalidInput { .. })
    ));
}
