//! Build the active provider list from CLI options and run discovery.
//!
//! Risk gating:
//! - `--category` overrides risk filtering. Naming a category counts as
//!   explicit consent; `--include-*` flags are ignored when `--category` is
//!   set.
//! - Otherwise: Safe by default; `--include-review` adds Review;
//!   `--include-destructive` adds Destructive.

use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;

use super::process::{AppProbe, PgrepChecker, ProcessChecker};
use super::providers::{all_providers, all_providers_with, CleanProvider};
use super::scan_context::{ScanContext, Unreadable};
use super::trash_plan::protected_reason;
use super::types::{CleanItem, RiskLevel};
use crate::options::CleanOptions as CleanOpts;
use crate::runner::CommandRunner;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryGroup {
    pub id: String,
    pub label: String,
    pub risk: RiskLevel,
    pub items: Vec<CleanItem>,
    pub total_size: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryReport {
    pub groups: Vec<CategoryGroup>,
    pub skipped_running: Vec<(String, String)>, // (category_id, app)
}

/// A discovered item plus what its size walk could not read; with
/// `unreadable` set, `item.size` is a lower bound.
#[derive(Debug, Clone)]
pub struct CheckedItem {
    pub item: CleanItem,
    pub unreadable: Option<Unreadable>,
}

#[derive(Debug, Clone)]
pub enum CategoryOutcome {
    Found {
        items: Vec<CheckedItem>,
        /// Listing/search paths with unreadable entries, outside any item.
        unreadable: Vec<(PathBuf, Unreadable)>,
        /// Directories discovery listed or walked for this category.
        roots: Vec<PathBuf>,
        /// Paths left out because they must never be cleaned, with why
        /// (`trash_plan::protected_reason`). Desktop discovery only.
        refused: Vec<(PathBuf, String)>,
    },
    /// The owning app is running, so the category was not scanned.
    AppRunning { app: String },
    /// A required tool or safety probe is unavailable.
    Unavailable { reason: String },
    /// The provider failed; other categories are unaffected.
    Failed { error: String },
}

#[derive(Debug, Clone)]
pub struct CheckedCategory {
    pub id: String,
    pub label: String,
    /// Why the category's items are candidates (`CleanProvider::inclusion_reason`).
    pub inclusion_reason: String,
    pub risk: RiskLevel,
    pub outcome: CategoryOutcome,
}

#[derive(Debug, Clone)]
pub struct CheckedDiscovery {
    pub categories: Vec<CheckedCategory>,
    /// Discovery stopped early; `categories` is incomplete.
    pub cancelled: bool,
}

/// Returns the providers selected by the CLI options. Filtering rules from
/// the plan flag matrix.
pub fn select_providers(opts: &CleanOpts) -> Vec<Box<dyn CleanProvider>> {
    filter_providers(opts, all_providers(opts))
}

/// `select_providers` with an explicit runner for tool-backed providers.
pub fn select_providers_with(
    opts: &CleanOpts,
    runner: Arc<dyn CommandRunner>,
) -> Vec<Box<dyn CleanProvider>> {
    filter_providers(opts, all_providers_with(opts, runner))
}

fn filter_providers(
    opts: &CleanOpts,
    all: Vec<Box<dyn CleanProvider>>,
) -> Vec<Box<dyn CleanProvider>> {
    if !opts.category.is_empty() {
        return all
            .into_iter()
            .filter(|p| opts.category.iter().any(|c| c == p.id()))
            .collect();
    }
    all.into_iter()
        .filter(|p| match p.risk() {
            RiskLevel::Safe => true,
            RiskLevel::Review => opts.include_review,
            RiskLevel::Destructive => opts.include_destructive,
        })
        .collect()
}

pub fn validate_options(opts: &CleanOpts) -> Result<()> {
    if opts.idle_days == 0 {
        return Err(crate::error::Error::InvalidInput(
            "idleDays must be greater than zero".into(),
        ));
    }
    Ok(())
}

pub fn discover(opts: &CleanOpts) -> Result<DiscoveryReport> {
    discover_with_progress(opts, None)
}

pub fn discover_with_checker(
    opts: &CleanOpts,
    checker: &dyn ProcessChecker,
) -> Result<DiscoveryReport> {
    discover_inner(opts, checker, None)
}

pub fn discover_with_progress(
    opts: &CleanOpts,
    progress: crate::progress::ProgressCallback<'_>,
) -> Result<DiscoveryReport> {
    validate_options(opts)?;
    discover_inner(opts, &PgrepChecker, progress)
}

/// Lenient probe for the compatibility path: a checker that cannot fail.
struct Lenient<'a>(&'a dyn ProcessChecker);

impl AppProbe for Lenient<'_> {
    fn probe(&self, name: &str) -> std::result::Result<bool, String> {
        Ok(self.0.is_running(name))
    }
}

/// The CLI's discovery. Unavailable and failed categories are dropped,
/// which is what the CLI showed before failures were reported.
fn discover_inner(
    opts: &CleanOpts,
    checker: &dyn ProcessChecker,
    progress: crate::progress::ProgressCallback<'_>,
) -> Result<DiscoveryReport> {
    let providers = select_providers(opts);
    let probe = Lenient(checker);
    let ctx = ScanContext::new(None, &probe);
    Ok(legacy_report(discover_checked(&providers, &ctx, progress)))
}

/// Maps checked discovery to the CLI's report: unavailable, failed and
/// empty categories are dropped, as the CLI showed nothing for them before.
fn legacy_report(checked: CheckedDiscovery) -> DiscoveryReport {
    let mut report = DiscoveryReport {
        groups: Vec::new(),
        skipped_running: Vec::new(),
    };
    for category in checked.categories {
        match category.outcome {
            CategoryOutcome::Found { items, .. } if !items.is_empty() => {
                let items: Vec<CleanItem> = items.into_iter().map(|c| c.item).collect();
                let total_size = items
                    .iter()
                    .fold(0u64, |sum, item| sum.saturating_add(item.size));
                report.groups.push(CategoryGroup {
                    id: category.id,
                    label: category.label,
                    risk: category.risk,
                    items,
                    total_size,
                });
            }
            CategoryOutcome::AppRunning { app } => {
                report.skipped_running.push((category.id, app));
            }
            _ => {}
        }
    }
    report
}

/// Runs every provider, isolating failures: a provider that errors, lacks
/// its tool or cannot check its app becomes a typed entry and the rest
/// still run. Stops between and inside providers once `ctx` is cancelled.
pub fn discover_checked(
    providers: &[Box<dyn CleanProvider>],
    ctx: &ScanContext<'_>,
    progress: crate::progress::ProgressCallback<'_>,
) -> CheckedDiscovery {
    let total = providers.len() as u64;
    let mut categories = Vec::new();
    for (index, provider) in providers.iter().enumerate() {
        if ctx.is_cancelled() {
            return CheckedDiscovery {
                categories,
                cancelled: true,
            };
        }
        crate::progress::report(
            progress,
            "scan",
            index as u64,
            Some(total),
            provider.label(),
        );
        let outcome = discover_one(provider.as_ref(), ctx);
        let findings = ctx.take_findings();
        if ctx.is_cancelled() {
            // The provider's walk stopped early; its result is partial.
            return CheckedDiscovery {
                categories,
                cancelled: true,
            };
        }
        let outcome = match outcome {
            Some(Ok(items)) => found(items, findings, |path| {
                let home = ctx.protected_home()?;
                protected_reason(path, Some(home), provider.roots_from_tool_output())
            }),
            Some(Err(outcome)) => outcome,
            None => continue,
        };
        // Empty categories stay: the desktop lists what was checked and found nothing.
        categories.push(CheckedCategory {
            id: provider.id().to_string(),
            label: provider.label().to_string(),
            inclusion_reason: provider.inclusion_reason(),
            risk: provider.risk(),
            outcome,
        });
    }
    crate::progress::report(progress, "scan", total, Some(total), "Discovery complete");
    CheckedDiscovery {
        categories,
        cancelled: false,
    }
}

/// `None`: the provider does not apply here (nothing to report).
fn discover_one(
    provider: &dyn CleanProvider,
    ctx: &ScanContext<'_>,
) -> Option<std::result::Result<Vec<CleanItem>, CategoryOutcome>> {
    if !provider.available() {
        let tool = provider.required_tool()?;
        return Some(Err(CategoryOutcome::Unavailable {
            reason: format!("{tool} was not found"),
        }));
    }
    for app in provider.quit_apps() {
        match ctx.app_running(app) {
            Ok(false) => {}
            Ok(true) => {
                return Some(Err(CategoryOutcome::AppRunning {
                    app: app.to_string(),
                }))
            }
            Err(e) => {
                return Some(Err(CategoryOutcome::Unavailable {
                    reason: e.to_string(),
                }))
            }
        }
    }
    Some(provider.discover(ctx).map_err(|e| CategoryOutcome::Failed {
        error: e.to_string(),
    }))
}

fn found(
    items: Vec<CleanItem>,
    findings: super::scan_context::ScanFindings,
    protected: impl Fn(&std::path::Path) -> Option<String>,
) -> CategoryOutcome {
    let mut unreadable = findings.unreadable;
    let mut refused = Vec::new();
    let mut kept = Vec::new();
    for item in items {
        match protected(&item.path) {
            Some(reason) => refused.push((item.path, reason)),
            None => kept.push(CheckedItem {
                unreadable: unreadable.remove(&item.path),
                item,
            }),
        }
    }
    CategoryOutcome::Found {
        items: kept,
        unreadable: unreadable.into_iter().collect(),
        roots: findings.roots.into_iter().collect(),
        refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::CleanOptions as CleanOpts;

    fn opts() -> CleanOpts {
        CleanOpts {
            category: Vec::new(),
            include_review: false,
            include_destructive: false,
            idle_days: 30,
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
                    assert_eq!(items.len(), 1);
                    assert_eq!(items[0].item.size, 5);
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
            let providers: Vec<Box<dyn CleanProvider>> =
                vec![Box::new(ToolFixture(MockRunner::new()))];
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
}
