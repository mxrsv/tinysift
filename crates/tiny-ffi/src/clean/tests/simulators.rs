//! Simulator data on the desktop (spec OC8–OC11, OC13): system caches carry
//! the administrator marker and are deleted only as administrator, through
//! the injected fake route; runtimes carry the reason only System Settings
//! can remove them. Temp dirs, fake runners and fake routes only.

use std::sync::Arc;

use tiny_core::clean::elevated_delete::ElevatedError;
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

/// `Caches/dyld` (root-owned on a real Mac) and `logs/a` (Rebuilt).
fn caches_layout(label: &str) -> PathBuf {
    let dir = fixture_dir(label, &[], 0);
    std::fs::create_dir_all(dir.join("Caches/dyld")).unwrap();
    std::fs::write(dir.join("Caches/dyld/cache"), vec![1u8; 9000]).unwrap();
    std::fs::create_dir(dir.join("logs")).unwrap();
    std::fs::write(dir.join("logs/a"), b"1234").unwrap();
    dir
}

fn caches_providers(dir: &Path) -> Vec<Box<dyn CleanProvider>> {
    vec![
        Box::new(SystemSimulatorCaches::with_root(
            dir.join("Caches"),
            Arc::new(NoSimctl),
        )),
        Box::new(Fixture::new("user-logs", &dir.join("logs"))),
    ]
}

/// `clean_remove` through the shared test routes: `FakeTrash` stands in for
/// Finder, deletion and deletion as administrator.
fn remove_with(
    session: &TinySession,
    preview_id: &str,
    dir: &Path,
    sinks: &FakeTrash,
) -> Result<FfiRemovalReport, FfiError> {
    let providers = |_: &CleanOptions| caches_providers(dir);
    session.remove_clean(
        preview_id,
        &providers,
        &NotRunning,
        sinks,
        sinks,
        sinks,
        &CancellationToken::default(),
        &Progress::default(),
    )
}

#[test]
fn system_simulator_caches_are_deleted_as_administrator_in_either_mode() {
    let dir = caches_layout("system-simulator-caches");
    let session = TestSession::new();
    let discovery = discover(&session, &caches_providers(&dir)).unwrap();
    let caches = &discovery.categories[0];
    assert_eq!(caches.id, "system-simulator-caches");
    assert_eq!(caches.status, FfiCategoryStatus::Found);
    assert!(caches.needs_admin);
    assert_eq!(
        caches.desktop_action,
        FfiDesktopAction::DeleteAsAdministrator
    );
    assert_eq!(caches.risk, FfiRisk::Safe);
    assert_eq!(caches.section, Some(FfiCleanSection::Rebuilt));
    assert_eq!(caches.comes_back, Some(FfiComesBack::AppRecreates));
    assert_eq!(caches.family.as_deref(), Some("dev"));
    let paths: Vec<_> = caches.candidates.iter().map(|c| c.path.clone()).collect();
    assert_eq!(paths, vec![dir.join("Caches/dyld").to_string_lossy()]);
    let size = allocated(&dir.join("Caches/dyld/cache"));
    assert_eq!(caches.candidates[0].size_bytes, size);
    assert!(!discovery.categories[1].needs_admin);
    let id = caches.candidates[0].id.clone();
    for mode in [FfiDeleteMode::Trash, FfiDeleteMode::Permanent] {
        let preview = session
            .clean_preview_with_mode(vec![id.clone()], mode)
            .unwrap();
        let item = &preview.items[0];
        assert!(item.permanent && item.needs_admin, "{mode:?}");
        assert_eq!(preview.bytes_needing_admin, size);
        assert_eq!(preview.bytes_to_free, size);
        assert_eq!(preview.bytes_to_trash, 0);
    }
    let logs = candidate_ids(&discovery, "user-logs");
    let preview = session.clean_preview(logs).unwrap();
    assert!(!preview.items[0].needs_admin);
    assert_eq!(preview.bytes_needing_admin, 0);

    // The old Trash-only call refuses it unspent; `clean_remove` runs it.
    let preview = session.clean_preview(vec![id]).unwrap();
    let providers = |_: &CleanOptions| caches_providers(&dir);
    let refused = session.execute_clean(
        &preview.preview_id,
        &providers,
        &NotRunning,
        &FakeTrash::default(),
        &CancellationToken::default(),
        &Progress::default(),
    );
    assert!(matches!(refused, Err(FfiError::InvalidInput { .. })));
    let sinks = FakeTrash::default();
    assert!(remove_with(&session, &preview.preview_id, &dir, &sinks).is_ok());
    assert_eq!(sinks.steps(), ["elevate dyld"]);
    remove(&dir);
}

#[test]
fn the_test_routes_stand_in_for_the_dialog_and_change_nothing() {
    let dir = caches_layout("elevated-fake");
    let session = TestSession::new();
    let discovery = discover(&session, &caches_providers(&dir)).unwrap();
    let ids = candidate_ids(&discovery, "system-simulator-caches");
    let preview = session.clean_preview(ids).unwrap();
    let sinks = FakeTrash::default();
    let report = remove_with(&session, &preview.preview_id, &dir, &sinks).unwrap();
    assert_eq!(sinks.steps(), ["elevate dyld"], "the fake got the batch");
    assert!(dir.join("Caches/dyld/cache").exists(), "nothing real ran");
    assert!(matches!(
        &report.results[0].outcome,
        FfiRemovalOutcome::Failed {
            reason: FfiRemovalFailure::DeleteFailed,
            source_still_present: true,
            ..
        }
    ));
    assert_eq!((report.bytes_freed, report.failed_count), (0, 1));
    assert!(!report.authentication_cancelled);
    remove(&dir);
}

#[test]
fn a_cancelled_authentication_changes_nothing_and_the_report_says_so() {
    let dir = caches_layout("elevated-cancelled");
    let session = TestSession::new();
    let discovery = discover(&session, &caches_providers(&dir)).unwrap();
    let preview = session
        .clean_preview_with_mode(ids(&discovery), FfiDeleteMode::Permanent)
        .unwrap();
    assert!(preview.items.iter().all(|i| i.permanent));
    let sinks = FakeTrash {
        elevate_answer: Some(ElevatedError::Cancelled),
        ..Default::default()
    };
    let report = remove_with(&session, &preview.preview_id, &dir, &sinks).unwrap();
    assert!(report.authentication_cancelled);
    assert_eq!(report.stopped, Some(FfiStopReason::Cancelled));
    let outcomes: Vec<_> = report.results.iter().map(|r| r.outcome.clone()).collect();
    assert_eq!(outcomes, vec![FfiRemovalOutcome::NotAttempted; 2]);
    assert_eq!(report.not_attempted_count, 2);
    assert_eq!(sinks.steps(), ["elevate dyld"], "the log is not deleted");
    assert!(dir.join("logs/a").exists() && dir.join("Caches/dyld/cache").exists());
    let again = remove_with(&session, &preview.preview_id, &dir, &sinks);
    assert!(
        matches!(again, Err(FfiError::PreviewInvalid { .. })),
        "the preview stays spent"
    );
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
