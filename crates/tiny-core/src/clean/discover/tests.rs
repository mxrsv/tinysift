use super::*;
use crate::options::CleanOptions as CleanOpts;

fn opts() -> CleanOpts {
    CleanOpts {
        category: Vec::new(),
        include_review: false,
        include_destructive: false,
        idle_days: 30,
        all_build_output: false,
        include_trash: false,
        tool_commands: false,
    }
}

#[test]
fn default_only_safe_providers() {
    let providers = select_providers(&opts());
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    assert!(ids.contains(&"user-logs"));
    assert!(ids.contains(&"xcode-derived"));
    assert!(!ids.contains(&"user-caches"));
    assert!(!ids.contains(&"trash"));
}

#[test]
fn the_cli_lists_root_owned_caches_only_when_named() {
    let ids = |o: &CleanOpts| {
        select_providers(o)
            .iter()
            .map(|p| p.id())
            .collect::<Vec<_>>()
    };
    let mut o = opts();
    o.include_review = true;
    assert!(!ids(&o).contains(&"system-simulator-caches"));
    o.category = vec!["system-simulator-caches".into()];
    assert_eq!(ids(&o), ["system-simulator-caches"]);
    let runner = Arc::new(crate::runner::test_support::MockRunner::new());
    let desktop = select_providers_with(&opts(), runner);
    let id = "system-simulator-caches";
    assert!(
        desktop.iter().any(|p| p.id() == id),
        "the app deletes it as administrator"
    );
}

#[test]
fn include_review_adds_review() {
    let mut o = opts();
    o.include_review = true;
    let providers = select_providers(&o);
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    assert!(ids.contains(&"user-caches"));
    assert!(ids.contains(&"cargo"));
    assert!(!ids.contains(&"trash"));
}

#[test]
fn include_destructive_adds_trash() {
    let mut o = opts();
    o.include_destructive = true;
    let providers = select_providers(&o);
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    assert!(ids.contains(&"trash"));
}

#[test]
fn the_trash_option_adds_only_the_trash() {
    let mut o = opts();
    o.include_review = true;
    o.include_trash = true;
    let providers = select_providers(&o);
    let destructive: Vec<&str> = providers
        .iter()
        .filter(|p| p.risk() == RiskLevel::Destructive)
        .map(|p| p.id())
        .collect();
    assert_eq!(destructive, ["trash"]);
}

#[test]
fn category_overrides_risk_filtering() {
    let mut o = opts();
    o.category = vec!["trash".into()];
    let providers = select_providers(&o);
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    assert_eq!(ids, vec!["trash"]);
}

#[test]
fn category_can_pick_review_without_flag() {
    let mut o = opts();
    o.category = vec!["cargo".into()];
    let providers = select_providers(&o);
    let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
    assert_eq!(ids, vec!["cargo"]);
}

#[test]
fn running_xcode_skips_xcode_categories_at_discovery() {
    use crate::clean::process::test_support::MockChecker;
    let mut o = opts();
    o.category = vec!["xcode-derived".into(), "xcode-archives".into()];
    let mock = MockChecker::with_running(["Xcode"]);
    let report = discover_with_checker(&o, &mock).unwrap();
    // Both Xcode categories must be skipped, not discovered.
    assert!(
        report.groups.iter().all(|g| g.id != "xcode-derived"),
        "xcode-derived should have been skipped"
    );
    assert!(
        report.groups.iter().all(|g| g.id != "xcode-archives"),
        "xcode-archives should have been skipped"
    );
    // And the skip must be reported.
    let skipped_apps: Vec<&str> = report
        .skipped_running
        .iter()
        .map(|(_, app)| app.as_str())
        .collect();
    assert!(skipped_apps.iter().all(|a| *a == "Xcode"));
    assert!(report
        .skipped_running
        .iter()
        .any(|(id, _)| id == "xcode-derived"));
    assert!(report
        .skipped_running
        .iter()
        .any(|(id, _)| id == "xcode-archives"));
}

mod checked {
    use super::super::*;
    use crate::clean::process::test_support::{FailingProbe, MockChecker};
    use crate::clean::providers::{root_as_item, run_tool, top_level_entries};
    use crate::clean::types::{ExecAction, ExecReport};
    use crate::runner::test_support::MockRunner;
    use crate::runner::CommandError;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Lists `root`'s children; `fail` makes discovery error instead.
    struct Fixture {
        id: &'static str,
        root: PathBuf,
        fail: bool,
        app: Option<&'static str>,
        cancel: Option<&'static AtomicBool>,
    }

    impl Fixture {
        fn new(id: &'static str, root: &Path) -> Self {
            Self {
                id,
                root: root.to_path_buf(),
                fail: false,
                app: None,
                cancel: None,
            }
        }
    }

    impl CleanProvider for Fixture {
        fn id(&self) -> &'static str {
            self.id
        }
        fn label(&self) -> &'static str {
            self.id
        }
        fn risk(&self) -> RiskLevel {
            RiskLevel::Safe
        }
        fn inclusion_reason(&self) -> String {
            "fixture".into()
        }
        fn requires_app_quit(&self) -> Option<&'static str> {
            self.app
        }
        fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
            if self.fail {
                return Err(crate::engine_error!("fixture failure"));
            }
            if let Some(flag) = self.cancel {
                flag.store(true, Ordering::Release);
            }
            Ok(top_level_entries(
                ctx,
                &self.root,
                self.id,
                self.id,
                RiskLevel::Safe,
            ))
        }
        fn execute(&self, _: &[CleanItem], _: ExecAction) -> Result<ExecReport> {
            unreachable!("discovery tests never execute")
        }
    }

    /// A tool-backed provider, like docker/go.
    struct ToolFixture(MockRunner);

    impl CleanProvider for ToolFixture {
        fn id(&self) -> &'static str {
            "tool"
        }
        fn label(&self) -> &'static str {
            "tool"
        }
        fn risk(&self) -> RiskLevel {
            RiskLevel::Safe
        }
        fn inclusion_reason(&self) -> String {
            "fixture".into()
        }
        fn available(&self) -> bool {
            self.0.which("fake-tool")
        }
        fn required_tool(&self) -> Option<&'static str> {
            Some("fake-tool")
        }
        fn roots_from_tool_output(&self) -> bool {
            true
        }
        fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
            let out = run_tool(&self.0, "fake-tool", &["path"])?;
            Ok(root_as_item(
                ctx,
                Path::new(out.stdout.trim()),
                "tool",
                "tool",
                RiskLevel::Safe,
            ))
        }
        fn execute(&self, _: &[CleanItem], _: ExecAction) -> Result<ExecReport> {
            unreachable!("discovery tests never execute")
        }
    }

    fn fixture_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tiny-discover-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("child")).unwrap();
        std::fs::write(dir.join("child/file"), b"12345").unwrap();
        dir
    }

    fn outcome<'a>(report: &'a CheckedDiscovery, id: &str) -> &'a CategoryOutcome {
        &report
            .categories
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .outcome
    }

    #[test]
    fn cli_report_keeps_its_old_shape() {
        let item = |size| CheckedItem {
            item: CleanItem {
                category_id: "a".into(),
                category_label: "a".into(),
                path: PathBuf::from(format!("/x/{size}")),
                size,
                risk: RiskLevel::Safe,
                evidence: Vec::new(),
            },
            unreadable: None,
            size_is_upper_bound: false,
            shared: SharedBlocks::default(),
        };
        let note = Unreadable {
            entries: 1,
            first_error: "denied".into(),
        };
        let category = |id: &str, outcome| CheckedCategory {
            id: id.into(),
            label: id.into(),
            inclusion_reason: String::new(),
            risk: RiskLevel::Safe,
            outcome,
        };
        let checked = CheckedDiscovery {
            categories: vec![
                category(
                    "found",
                    CategoryOutcome::Found {
                        items: vec![item(3), item(4)],
                        unreadable: Vec::new(),
                        roots: Vec::new(),
                        refused: Vec::new(),
                    },
                ),
                category(
                    "denied-only",
                    CategoryOutcome::Found {
                        items: Vec::new(),
                        unreadable: vec![(PathBuf::from("/m"), note)],
                        roots: Vec::new(),
                        refused: Vec::new(),
                    },
                ),
                category(
                    "running",
                    CategoryOutcome::AppRunning { app: "Mail".into() },
                ),
                category(
                    "docker",
                    CategoryOutcome::Failed {
                        error: "daemon down".into(),
                    },
                ),
                category(
                    "go",
                    CategoryOutcome::Unavailable {
                        reason: "go was not found".into(),
                    },
                ),
            ],
            cancelled: false,
        };
        let report = legacy_report(checked);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].id, "found");
        assert_eq!(report.groups[0].total_size, 7);
        assert_eq!(
            report.skipped_running,
            vec![("running".to_string(), "Mail".to_string())]
        );
    }

    #[test]
    fn failing_provider_is_isolated_from_healthy_ones() {
        use std::os::unix::fs::MetadataExt;
        let root = fixture_root("isolate");
        let mut failing = Fixture::new("bad", &root);
        failing.fail = true;
        let providers: Vec<Box<dyn CleanProvider>> =
            vec![Box::new(failing), Box::new(Fixture::new("good", &root))];
        let probe = MockChecker::none();
        let ctx = ScanContext::new(None, &probe);
        let report = discover_checked(&providers, &ctx, None);
        assert!(!report.cancelled);
        assert!(
            matches!(outcome(&report, "bad"), CategoryOutcome::Failed { error } if error.contains("fixture failure"))
        );
        match outcome(&report, "good") {
            CategoryOutcome::Found { items, roots, .. } => {
                let file = std::fs::symlink_metadata(root.join("child/file")).unwrap();
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].item.size, file.blocks() * 512);
                assert!(items[0].unreadable.is_none());
                assert_eq!(roots, &vec![root.clone()]);
            }
            other => panic!("unexpected {other:?}"),
        }
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn a_category_that_finds_nothing_is_still_reported() {
        let missing = std::env::temp_dir().join("tiny-discover-nothing-here-missing");
        let providers: Vec<Box<dyn CleanProvider>> =
            vec![Box::new(Fixture::new("empty", &missing))];
        let ctx = ScanContext::unchecked();
        let report = discover_checked(&providers, &ctx, None);
        assert!(
            matches!(outcome(&report, "empty"), CategoryOutcome::Found { items, .. } if items.is_empty())
        );
    }

    #[test]
    fn missing_tool_is_reported_unavailable() {
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(ToolFixture(MockRunner::new()))];
        let ctx = ScanContext::unchecked();
        let report = discover_checked(&providers, &ctx, None);
        assert!(
            matches!(outcome(&report, "tool"), CategoryOutcome::Unavailable { reason } if reason.contains("fake-tool"))
        );
    }

    #[test]
    fn a_tool_printing_home_or_a_path_outside_it_is_refused_on_the_desktop_only() {
        let home = fixture_root("tool-home");
        let printed = |path: &Path, desktop: bool| {
            let runner = MockRunner::new().with_which("fake-tool").with_exit(
                "fake-tool",
                &["path"],
                0,
                &format!("{}\n", path.display()),
            );
            let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(ToolFixture(runner))];
            let ctx = ScanContext::unchecked();
            let ctx = if desktop {
                ctx.refusing_protected_paths(home.clone())
            } else {
                ctx
            };
            let report = discover_checked(&providers, &ctx, None);
            match outcome(&report, "tool").clone() {
                CategoryOutcome::Found { items, refused, .. } => (items.len(), refused),
                other => panic!("unexpected {other:?}"),
            }
        };
        let (found, refused) = printed(&home, true);
        assert_eq!((found, refused[0].1.as_str()), (0, "the home folder"));
        // Outside `home` (an ancestor here): refused on the desktop ...
        let (found, refused) = printed(&std::env::temp_dir(), true);
        assert_eq!((found, refused.len()), (0, 1));
        // ... but the CLI lists it exactly as before.
        let (found, refused) = printed(&std::env::temp_dir(), false);
        assert_eq!((found, refused.len()), (1, 0));
        let (found, refused) = printed(&home, false);
        assert_eq!((found, refused.len()), (1, 0));
        let (found, refused) = printed(&home.join("child"), true);
        assert_eq!((found, refused.len()), (1, 0));
        let _ = crate::clean::fs_safe::remove_recursive_safe(&home);
    }

    #[test]
    fn child_command_timeout_fails_the_category() {
        let runner = MockRunner::new().with_which("fake-tool").with_output(
            "fake-tool",
            &["path"],
            Err(CommandError::Timeout {
                bin: "fake-tool".into(),
                timeout_ms: 30_000,
            }),
        );
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(ToolFixture(runner))];
        let ctx = ScanContext::unchecked();
        let report = discover_checked(&providers, &ctx, None);
        assert!(
            matches!(outcome(&report, "tool"), CategoryOutcome::Failed { error } if error.contains("did not finish"))
        );
    }

    /// Gated on two apps through `quit_apps`, like the simulator categories.
    struct TwoApps;

    impl CleanProvider for TwoApps {
        fn id(&self) -> &'static str {
            "two-apps"
        }
        fn label(&self) -> &'static str {
            "two-apps"
        }
        fn risk(&self) -> RiskLevel {
            RiskLevel::Safe
        }
        fn inclusion_reason(&self) -> String {
            "fixture".into()
        }
        fn quit_apps(&self) -> Vec<&'static str> {
            vec!["Xcode", "Simulator"]
        }
        fn discover(&self, _: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
            Ok(Vec::new())
        }
        fn execute(&self, _: &[CleanItem], _: ExecAction) -> Result<ExecReport> {
            unreachable!("discovery tests never execute")
        }
    }

    #[test]
    fn any_running_quit_app_refuses_the_category() {
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(TwoApps)];
        for app in ["Xcode", "Simulator"] {
            let running = MockChecker::with_running([app]);
            let ctx = ScanContext::new(None, &running);
            let report = discover_checked(&providers, &ctx, None);
            assert!(
                matches!(outcome(&report, "two-apps"), CategoryOutcome::AppRunning { app: a } if a == app),
                "{app}"
            );
        }
        let idle = MockChecker::none();
        let ctx = ScanContext::new(None, &idle);
        let report = discover_checked(&providers, &ctx, None);
        assert!(matches!(
            outcome(&report, "two-apps"),
            CategoryOutcome::Found { .. }
        ));
        assert_eq!(
            TwoApps.item_apps(Path::new("/x")),
            vec!["Xcode", "Simulator"]
        );
    }

    #[test]
    fn failed_running_app_probe_refuses_the_category() {
        let root = fixture_root("probe");
        let mut gated = Fixture::new("gated", &root);
        gated.app = Some("Xcode");
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(gated)];
        let ctx = ScanContext::new(None, &FailingProbe);
        let report = discover_checked(&providers, &ctx, None);
        assert!(
            matches!(outcome(&report, "gated"), CategoryOutcome::Unavailable { reason } if reason.contains("Xcode"))
        );
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn an_upper_bound_size_is_attached_to_its_item() {
        let item = |path: &str| CleanItem {
            category_id: "a".into(),
            category_label: "a".into(),
            path: PathBuf::from(path),
            size: 1,
            risk: RiskLevel::Safe,
            evidence: Vec::new(),
        };
        let mut findings = crate::clean::scan_context::ScanFindings::default();
        findings.size_upper_bound.insert(PathBuf::from("/x/hfs"));
        match found(vec![item("/x/hfs"), item("/x/apfs")], findings, |_| None) {
            CategoryOutcome::Found { items, .. } => {
                let flags: Vec<bool> = items.iter().map(|i| i.size_is_upper_bound).collect();
                assert_eq!(flags, vec![true, false]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn denied_reads_are_attached_to_the_item() {
        use std::os::unix::fs::PermissionsExt;
        let root = fixture_root("denied");
        let locked = root.join("child/locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("d", &root))];
        let ctx = ScanContext::unchecked();
        let report = discover_checked(&providers, &ctx, None);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        match outcome(&report, "d") {
            CategoryOutcome::Found { items, .. } => {
                assert_eq!(items[0].unreadable.as_ref().map(|u| u.entries), Some(1));
            }
            other => panic!("unexpected {other:?}"),
        }
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn cancellation_inside_a_provider_discards_its_partial_result() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let root = fixture_root("cancel");
        let mut cancelling = Fixture::new("first", &root);
        cancelling.cancel = Some(&FLAG);
        let providers: Vec<Box<dyn CleanProvider>> = vec![
            Box::new(cancelling),
            Box::new(Fixture::new("second", &root)),
        ];
        let probe = MockChecker::none();
        let ctx = ScanContext::new(Some(&FLAG), &probe);
        let report = discover_checked(&providers, &ctx, None);
        assert!(report.cancelled);
        assert!(report.categories.is_empty());
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }
}

#[test]
fn cli_keeps_failed_unavailable_and_running_statuses_outside_legacy_json() {
    let category = |outcome| CheckedCategory {
        id: "fixture".into(),
        label: "Fixture".into(),
        inclusion_reason: String::new(),
        risk: RiskLevel::Safe,
        outcome,
    };
    for outcome in [
        CategoryOutcome::Failed {
            error: "fixture failure".into(),
        },
        CategoryOutcome::Unavailable {
            reason: "probe failed".into(),
        },
        CategoryOutcome::AppRunning {
            app: "FixtureApp".into(),
        },
    ] {
        let mut options = opts();
        options.category = vec!["fixture".into()];
        let result = cli_discovery(
            CheckedDiscovery {
                categories: vec![category(outcome)],
                cancelled: false,
            },
            &options,
            &[],
        );
        assert!(result.failed);
        assert_eq!(result.diagnostics.len(), 1);
        let json = serde_json::to_value(result.report).unwrap();
        assert!(json.get("groups").is_some());
        assert!(json.get("skippedRunning").is_some());
        assert!(json.get("diagnostics").is_none());
    }
}

#[test]
fn cli_adapter_refuses_both_category_and_candidate_unreadable_results() {
    let unreadable = || Unreadable {
        entries: 1,
        first_error: "fixture permission denied".into(),
    };
    for candidate_partial in [false, true] {
        let outcome = CategoryOutcome::Found {
            items: if candidate_partial {
                vec![CheckedItem {
                    item: CleanItem {
                        category_id: "fixture".into(),
                        category_label: "Fixture".into(),
                        path: "/fixture/candidate".into(),
                        size: 5,
                        risk: RiskLevel::Safe,
                        evidence: Vec::new(),
                    },
                    unreadable: Some(unreadable()),
                    size_is_upper_bound: false,
                    shared: SharedBlocks::default(),
                }]
            } else {
                Vec::new()
            },
            unreadable: if candidate_partial {
                Vec::new()
            } else {
                vec![(PathBuf::from("/fixture/unreadable"), unreadable())]
            },
            roots: vec![PathBuf::from("/fixture")],
            refused: Vec::new(),
        };
        let result = cli_discovery(
            CheckedDiscovery {
                categories: vec![CheckedCategory {
                    id: "fixture".into(),
                    label: "Fixture".into(),
                    inclusion_reason: String::new(),
                    risk: RiskLevel::Safe,
                    outcome,
                }],
                cancelled: false,
            },
            &opts(),
            &[],
        );
        assert!(
            result.failed,
            "partial candidate={candidate_partial} must refuse cleanup"
        );
        assert!(result
            .diagnostics
            .iter()
            .any(|message| message.contains("fixture")
                && message.contains("could not be fully read")));
        if candidate_partial {
            assert_eq!(
                result.report.groups[0].items[0].size, 5,
                "retain the measured lower bound rather than invent zero"
            );
        }
    }
}
