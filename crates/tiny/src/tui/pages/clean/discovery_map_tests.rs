use std::path::PathBuf;

use tiny_core::clean::discover::{CategoryGroup, DiscoveryReport};
use tiny_core::clean::providers::CleanProvider;
use tiny_core::clean::scan_context::ScanContext;
use tiny_core::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

use super::*;

/// A provider that answers only what the mapping asks.
struct Fixture {
    id: &'static str,
    risk: RiskLevel,
    trash_paths: bool,
    admin: bool,
    report_only: ReportOnly,
}

impl Fixture {
    fn new(id: &'static str, risk: RiskLevel) -> Self {
        Self {
            id,
            risk,
            trash_paths: true,
            admin: false,
            report_only: ReportOnly::NotPerPathTrash,
        }
    }

    fn tool(self) -> Self {
        Self {
            trash_paths: false,
            ..self
        }
    }

    fn report_only(self, why: ReportOnly) -> Self {
        Self {
            trash_paths: false,
            report_only: why,
            ..self
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
    fn inclusion_reason(&self) -> String {
        format!("why {}", self.id)
    }
    fn risk(&self) -> RiskLevel {
        self.risk
    }
    fn discover(&self, _ctx: &ScanContext<'_>) -> tiny_core::error::Result<Vec<CleanItem>> {
        Ok(Vec::new())
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> tiny_core::error::Result<ExecReport> {
        unreachable!("the mapping never executes")
    }
    fn desktop_trash_paths(&self) -> bool {
        self.trash_paths
    }
    fn desktop_report_only_reason(&self) -> ReportOnly {
        self.report_only
    }
    fn needs_admin(&self) -> bool {
        self.admin
    }
}

fn group(id: &str) -> CategoryGroup {
    CategoryGroup {
        id: id.into(),
        label: id.into(),
        risk: RiskLevel::Safe,
        items: vec![CleanItem {
            category_id: id.into(),
            category_label: id.into(),
            path: PathBuf::from(format!("/x/{id}")),
            size: 1,
            risk: RiskLevel::Safe,
            evidence: Vec::new(),
        }],
        total_size: 1,
    }
}

fn found(groups: &[&str], running: &[(&str, &str)], failed: &[(&str, &str)]) -> CliDiscovery {
    CliDiscovery {
        report: DiscoveryReport {
            groups: groups.iter().map(|id| group(id)).collect(),
            skipped_running: running
                .iter()
                .map(|(id, app)| (id.to_string(), app.to_string()))
                .collect(),
        },
        diagnostics: Vec::new(),
        failed: !failed.is_empty(),
        failed_categories: failed
            .iter()
            .map(|(id, why)| (id.to_string(), why.to_string()))
            .collect(),
    }
}

#[test]
fn every_report_only_answer_has_one_route_and_destructive_has_none() {
    assert_eq!(route(None), Some(Route::Trash));
    assert_eq!(
        route(Some(ReportOnly::NotPerPathTrash)),
        Some(Route::ForGood)
    );
    assert_eq!(route(Some(ReportOnly::Destructive)), None);
    for why in [
        ReportOnly::UnreliableMatch,
        ReportOnly::NeedsAdministrator,
        ReportOnly::XcodeNotInstalled,
    ] {
        assert_eq!(route(Some(why)), Some(Route::ReportOnly), "{why:?}");
    }
}

#[test]
fn providers_map_to_their_routes_and_destructive_is_hidden() {
    let providers: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(Fixture::new("logs", RiskLevel::Safe)),
        Box::new(Fixture::new("pnpm", RiskLevel::Safe).tool()),
        Box::new(
            Fixture::new("admin", RiskLevel::Safe).report_only(ReportOnly::NeedsAdministrator),
        ),
        Box::new(Fixture {
            admin: true,
            ..Fixture::new("sudo", RiskLevel::Safe)
        }),
        Box::new(Fixture::new("trash", RiskLevel::Destructive)),
    ];
    let all = ["logs", "pnpm", "admin", "sudo", "trash"];
    let categories = categories(&providers, &found(&all, &[], &[]));
    let routes: Vec<(&str, Route)> = categories
        .iter()
        .map(|c| (c.id.as_str(), c.route))
        .collect();
    assert_eq!(
        routes,
        [
            ("logs", Route::Trash),
            ("pnpm", Route::ForGood),
            ("admin", Route::ReportOnly),
            ("sudo", Route::ReportOnly),
        ]
    );
    assert_eq!(categories[0].reason, "why logs");
}

#[test]
fn a_failure_wins_over_what_was_found_and_found_over_a_running_app() {
    let both = found(
        &["a", "b"],
        &[("a", "Xcode"), ("b", "Xcode"), ("c", "Docker")],
        &[("a", "denied")],
    );
    assert!(matches!(finding("a", &both), Some(Finding::Failed { reason }) if reason == "denied"));
    assert!(matches!(finding("b", &both), Some(Finding::Found(group)) if group.id == "b"));
    assert!(matches!(finding("c", &both), Some(Finding::AppRunning { app }) if app == "Docker"));
    assert!(finding("d", &both).is_none(), "nothing found: not listed");
}

#[test]
fn the_failure_is_the_first_diagnostic_or_a_plain_sentence() {
    let mut read = found(&["a"], &[], &[]);
    assert_eq!(failure(&read), None);
    read.failed = true;
    assert_eq!(failure(&read).as_deref(), Some(UNNAMED_FAILURE));
    read.diagnostics = vec!["a: permission denied".into(), "b: other".into()];
    assert_eq!(failure(&read).as_deref(), Some("a: permission denied"));
}

/// TU8: with every provider `sift clean` selects found, the page opens with
/// exactly the categories the command lists, ticked as its picker ticks
/// them, less those the engine only reports (it never acts on them).
#[test]
fn the_opening_set_is_what_plain_sift_clean_selects() {
    use super::super::engine::{clean_options, Discovery};
    use super::super::listing::Listing;
    use tiny_core::clean::discover::select_providers;
    use tiny_core::clean::providers::desktop_report_only;

    let providers = select_providers(&clean_options(false));
    let groups: Vec<CategoryGroup> = providers
        .iter()
        .map(|p| CategoryGroup {
            risk: p.risk(),
            label: p.label().into(),
            ..group(p.id())
        })
        .collect();
    let read = CliDiscovery {
        report: DiscoveryReport {
            groups: groups.clone(),
            skipped_running: Vec::new(),
        },
        diagnostics: Vec::new(),
        failed: false,
        failed_categories: Vec::new(),
    };
    let listing = Listing::new(
        Discovery {
            categories: categories(&providers, &read),
            ..Discovery::default()
        },
        None,
    );
    let mut listed: Vec<&str> = listing
        .entries
        .iter()
        .map(|e| e.category.id.as_str())
        .collect();
    listed.sort_unstable();
    let mut command_lists: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    command_lists.sort_unstable();
    assert_eq!(
        listed, command_lists,
        "the command's list, nothing more or less"
    );

    let command_ticks = crate::render::clean::picker::default_ticks(&groups);
    let acts = |id: &str| {
        providers
            .iter()
            .find(|p| p.id() == id)
            .is_some_and(|p| route(desktop_report_only(p.as_ref())) != Some(Route::ReportOnly))
    };
    let mut expected: Vec<&str> = command_ticks
        .iter()
        .map(|&i| groups[i].id.as_str())
        .filter(|id| acts(id))
        .collect();
    expected.sort_unstable();
    let mut ticked: Vec<&str> = listing.picked().map(|e| e.category.id.as_str()).collect();
    ticked.sort_unstable();
    assert!(!ticked.is_empty());
    assert_eq!(ticked, expected);
}
