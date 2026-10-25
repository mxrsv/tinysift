use super::*;

#[test]
fn every_known_category_has_family() {
    for id in known_category_ids() {
        // Must not panic. Result discarded; coverage is what we test.
        let _ = category_family(id);
    }
}

#[test]
fn family_id_and_label_stable() {
    assert_eq!(Family::Dev.id(), "dev");
    assert_eq!(Family::UserStorage.id(), "user-storage");
    assert_eq!(Family::System.id(), "system");
    assert_eq!(Family::Dev.label(), "Dev caches");
    assert_eq!(Family::UserStorage.label(), "User storage");
    assert_eq!(Family::System.label(), "System leftovers");
}

/// Deliberate desktop decision for every category. A new provider fails
/// this test until it is added here, so eligibility is never implicit.
const DESKTOP_TRASH: &[(&str, Option<ReportOnly>)] = &[
    ("user-logs", None),
    ("xcode-derived", None),
    ("user-caches", None),
    ("xcode-archives", None),
    ("xcode-devicesupport", None),
    ("cargo", None),
    ("npm", None),
    // Runs `pnpm store prune`, not a per-path move.
    ("pnpm", Some(ReportOnly::NotPerPathTrash)),
    ("yarn", None),
    // Runs `uv cache prune`, not a per-path move.
    ("uv", Some(ReportOnly::NotPerPathTrash)),
    ("puppeteer", None),
    ("huggingface", None),
    ("codex-runtimes", None),
    ("codex-releases", None),
    ("node-modules", None),
    ("python-caches", None),
    ("rust-targets", None),
    ("gradle-maven", None),
    ("jetbrains", None),
    ("vscode", None),
    ("ios-simulators", None),
    // Destructive: `xcrun simctl delete` removes the device for good.
    ("simulator-devices", Some(ReportOnly::Destructive)),
    // Root-owned: no unprivileged route; the app deletes them as
    // administrator, the CLI refuses them (no sudo).
    (
        "system-simulator-caches",
        Some(ReportOnly::NeedsAdministrator),
    ),
    // Runs `xcrun simctl runtime delete`, not a per-path move.
    ("simulator-runtimes", Some(ReportOnly::NotPerPathTrash)),
    // Without Xcode only System Settings > General > Storage removes
    // them; TinySift never changes /System/Library/AssetsV2.
    (
        "simulator-runtime-images",
        Some(ReportOnly::XcodeNotInstalled),
    ),
    ("android-sdk", None),
    ("go-cache", None),
    // Runs `docker image prune` / `docker builder prune`, not a per-path move.
    ("docker", Some(ReportOnly::NotPerPathTrash)),
    // Destructive: `docker volume prune` deletes container data.
    ("docker-volumes", Some(ReportOnly::Destructive)),
    ("downloads-old", None),
    ("screenshots-old", None),
    ("mail-attachments", None),
    ("streaming-caches", None),
    ("chat-caches", None),
    ("browser-caches", None),
    ("quarantine", None),
    ("crash-reports", None),
    // Folder names are matched against bundle IDs, so live data such as
    // `Code` (VS Code) or `AddressBook` is flagged as orphaned.
    ("app-orphans", Some(ReportOnly::UnreliableMatch)),
    // Destructive: `tmutil deletelocalsnapshots` and Empty Trash.
    ("time-machine-local", Some(ReportOnly::Destructive)),
    ("font-quicklook-caches", None),
    // Never moved or deleted path by path; the app may empty it as a
    // whole when it asks for the row (`desktop_empties_trash`).
    ("trash", Some(ReportOnly::Destructive)),
];

#[test]
fn every_category_has_a_deliberate_desktop_decision() {
    let decided: Vec<&str> = DESKTOP_TRASH.iter().map(|(id, _)| *id).collect();
    assert_eq!(decided, known_category_ids());
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::runner::test_support::MockRunner::new());
    let providers = all_providers_with(&crate::options::CleanOptions::default(), runner);
    assert_eq!(providers.len(), DESKTOP_TRASH.len());
    for provider in providers {
        let expected = DESKTOP_TRASH
            .iter()
            .find(|(id, _)| *id == provider.id())
            .map(|(_, value)| *value);
        assert_eq!(
            Some(desktop_report_only(provider.as_ref())),
            expected,
            "{}",
            provider.id()
        );
    }
}

#[test]
fn only_the_trash_can_be_emptied_from_the_desktop() {
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::runner::test_support::MockRunner::new());
    let providers = all_providers_with(&crate::options::CleanOptions::default(), runner);
    let emptied: Vec<&str> = providers
        .iter()
        .filter(|p| p.desktop_empties_trash())
        .map(|p| p.id())
        .collect();
    assert_eq!(emptied, ["trash"]);
}

#[test]
fn every_category_states_why_its_items_are_candidates() {
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::runner::test_support::MockRunner::new());
    let opts = crate::options::CleanOptions {
        idle_days: 45,
        ..Default::default()
    };
    let providers = all_providers_with(&opts, runner);
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    assert_eq!(ids, known_category_ids());
    for provider in &providers {
        let reason = provider.inclusion_reason();
        assert!(!reason.trim().is_empty(), "{} has no reason", provider.id());
    }
    let reason_of = |id: &str| {
        providers
            .iter()
            .find(|p| p.id() == id)
            .unwrap()
            .inclusion_reason()
    };
    for idle in [
        "node-modules",
        "rust-targets",
        "python-caches",
        "downloads-old",
    ] {
        assert!(
            reason_of(idle).contains("45 days"),
            "{idle} names its threshold"
        );
    }
}

/// A per-path provider that would let the desktop move its items.
struct NeedsAdmin;

impl CleanProvider for NeedsAdmin {
    fn id(&self) -> &'static str {
        "needs-admin"
    }
    fn label(&self) -> &'static str {
        "needs-admin"
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn discover(&self, _: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(Vec::new())
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> Result<ExecReport> {
        panic!("never executed")
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn needs_admin(&self) -> bool {
        true
    }
}

#[test]
fn a_category_that_needs_admin_has_no_unprivileged_desktop_route() {
    assert_eq!(
        desktop_report_only(&NeedsAdmin),
        Some(ReportOnly::NeedsAdministrator)
    );
}

#[test]
fn the_per_item_executor_never_acts_inside_system() {
    // Paths that do not exist: nothing on disk is touched either way.
    let items: Vec<CleanItem> = [
        "/System/Library/AssetsV2/tiny-test-missing/x.dmg",
        "/system/library/assetsv2/tiny-test-missing/x.dmg",
        "/Library/../System/Library/AssetsV2/tiny-test-missing/x.dmg",
    ]
    .into_iter()
    .map(|path| CleanItem {
        category_id: "user-logs".into(),
        category_label: "user-logs".into(),
        path: path.into(),
        size: 0,
        risk: RiskLevel::Safe,
        evidence: Vec::new(),
    })
    .collect();
    let report = execute_per_item(&items, ExecAction::HardDelete, "user-logs").unwrap();
    assert!(
        report.removed_paths.is_empty(),
        "{:?}",
        report.removed_paths
    );
    assert_eq!(report.failed.len(), items.len());
}

#[test]
fn per_path_app_gates_name_the_owning_app() {
    let h = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let browser = browser_caches::BrowserCaches::new();
    let chrome = h.join("Library/Application Support/Google/Chrome/Default/Cache");
    assert_eq!(browser.item_apps(&chrome), vec!["Google Chrome"]);
    assert!(browser.item_apps(Path::new("/elsewhere")).len() > 1);
    let caches = user_caches::UserCaches::with_runner(Arc::new(
        crate::runner::test_support::MockRunner::new(),
    ));
    assert_eq!(
        caches.item_apps(&h.join("Library/Caches/com.apple.Safari")),
        vec!["Safari"]
    );
    assert_eq!(
        xcode::XcodeDerivedData.item_apps(Path::new("/x")),
        vec!["Xcode"]
    );
}

#[test]
fn cargo_guard_refuses_everything_but_the_pinned_cache_dirs() {
    // Exercises the guard only; no execute or Trash path runs.
    let h = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let cargo = dev_caches::CargoCache;
    for pinned in ["registry/cache", "registry/src", "git/db", "git/checkouts"] {
        assert!(cargo.check_item(&h.join(".cargo").join(pinned)).is_ok());
    }
    for other in [".cargo/bin", ".cargo/credentials.toml", ".cargo", ".rustup"] {
        assert!(cargo.check_item(&h.join(other)).is_err(), "{other}");
    }
}

#[test]
fn every_category_says_how_its_items_come_back() {
    for id in known_category_ids() {
        assert!(comes_back(id).is_some(), "{id}");
    }
    assert_eq!(comes_back("not-a-category"), None);
}

/// Deliberate desktop section for every category. A new provider fails
/// this test until it is added here, so build output never lands in
/// Rebuilt just because `comes_back` says a build restores it.
const SECTIONS: &[(&str, Section)] = &[
    ("user-logs", Section::Rebuilt),
    ("xcode-derived", Section::Rebuilt),
    ("user-caches", Section::Rebuilt),
    ("xcode-archives", Section::YourFiles),
    ("xcode-devicesupport", Section::Rebuilt),
    ("cargo", Section::Rebuilt),
    ("npm", Section::Rebuilt),
    ("pnpm", Section::Rebuilt),
    ("yarn", Section::Rebuilt),
    ("uv", Section::Rebuilt),
    ("puppeteer", Section::Rebuilt),
    ("huggingface", Section::Rebuilt),
    ("codex-runtimes", Section::Rebuilt),
    ("codex-releases", Section::Rebuilt),
    ("node-modules", Section::BuildOutput),
    // Keeps its idle rule on the desktop too (`all_build_output` does not
    // reach it), so it is not listed per project like build output.
    ("python-caches", Section::Rebuilt),
    ("rust-targets", Section::BuildOutput),
    ("gradle-maven", Section::Rebuilt),
    ("jetbrains", Section::Rebuilt),
    ("vscode", Section::Rebuilt),
    ("ios-simulators", Section::Rebuilt),
    ("simulator-devices", Section::YourFiles),
    ("system-simulator-caches", Section::Rebuilt),
    ("simulator-runtimes", Section::Rebuilt),
    ("simulator-runtime-images", Section::Rebuilt),
    ("android-sdk", Section::Rebuilt),
    ("go-cache", Section::Rebuilt),
    ("docker", Section::Rebuilt),
    ("docker-volumes", Section::YourFiles),
    ("downloads-old", Section::YourFiles),
    ("screenshots-old", Section::YourFiles),
    ("mail-attachments", Section::YourFiles),
    ("streaming-caches", Section::Rebuilt),
    ("chat-caches", Section::Rebuilt),
    ("browser-caches", Section::Rebuilt),
    ("quarantine", Section::Rebuilt),
    ("crash-reports", Section::Rebuilt),
    ("app-orphans", Section::YourFiles),
    ("time-machine-local", Section::YourFiles),
    ("font-quicklook-caches", Section::Rebuilt),
    ("trash", Section::YourFiles),
];

#[test]
fn every_category_has_a_deliberate_desktop_section() {
    let decided: Vec<&str> = SECTIONS.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        decided,
        known_category_ids(),
        "every category needs a deliberate section in SECTIONS"
    );
    for (id, section) in SECTIONS {
        assert_eq!(category_section(id), Some(*section), "{id}");
    }
    assert_eq!(category_section("not-a-category"), None);
}

#[test]
fn only_the_desktop_option_lists_every_projects_build_output() {
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::runner::test_support::MockRunner::new());
    let labels = |all_build_output| {
        let opts = crate::options::CleanOptions {
            all_build_output,
            ..Default::default()
        };
        all_providers_with(&opts, runner.clone())
            .into_iter()
            .filter(|p| matches!(p.id(), "node-modules" | "rust-targets"))
            .map(|p| (p.label(), p.risk()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        labels(false),
        [
            ("node_modules (idle)", RiskLevel::Review),
            ("Rust target/ (idle)", RiskLevel::Review)
        ]
    );
    assert_eq!(
        labels(true),
        [
            ("node_modules", RiskLevel::Review),
            ("Rust target/", RiskLevel::Review)
        ]
    );
}

#[test]
#[should_panic(expected = "unknown category id")]
fn category_family_panics_on_unknown() {
    let _ = category_family("definitely-not-a-category");
}

#[test]
fn tool_ids_are_owned_by_exactly_the_expected_provider() {
    let providers = all_providers_with(
        &crate::options::CleanOptions::default(),
        Arc::new(crate::runner::test_support::MockRunner::new()),
    );
    for (owner, path) in [
        ("docker", "<docker:images>"),
        ("docker-volumes", "<docker:volumes>"),
        (
            "time-machine-local",
            "<tmutil:com.apple.TimeMachine.2025-01-01-000000.local>",
        ),
        (
            "simulator-runtimes",
            "<simulator-runtime:0A8C5D2E-1111-4A1B-8C2D-3E4F5A6B7C8D>",
        ),
    ] {
        for provider in &providers {
            assert_eq!(
                provider.owns_tool_id(Path::new(path)),
                provider.id() == owner,
                "{}: {path}",
                provider.id()
            );
        }
    }
    for provider in providers {
        for malformed in [
            "relative/file",
            "<simulator-runtime:../../Documents>",
            "<tmutil:com.apple.TimeMachine.../../x.local>",
        ] {
            assert!(!provider.owns_tool_id(Path::new(malformed)));
        }
    }
}
