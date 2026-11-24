//! The mock's data (`DATA` in `tools/mock.js`) as engine categories, for the fake engine.

use std::path::PathBuf;

use tiny_core::clean::discover::CategoryGroup;
use tiny_core::clean::types::{CleanItem, ComesBack, Evidence, RiskLevel};

use super::engine::{Category, Finding, Route};

pub const HOME: &str = "/Users/you";
/// When the fake's reads run: Unix seconds.
pub const READ_AT: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

pub fn home(rest: &str) -> String {
    format!("{HOME}/{rest}")
}

/// A category found with `items` (`path`, bytes).
pub fn found(
    id: &str,
    label: &str,
    risk: RiskLevel,
    route: Route,
    back: ComesBack,
    reason: &str,
    items: Vec<(String, u64)>,
) -> Category {
    let items: Vec<CleanItem> = items
        .into_iter()
        .map(|(path, size)| CleanItem {
            category_id: id.into(),
            category_label: label.into(),
            path: PathBuf::from(path),
            size,
            risk,
            evidence: Vec::new(),
        })
        .collect();
    Category {
        id: id.into(),
        label: label.into(),
        risk,
        reason: reason.into(),
        comes_back: Some(back),
        route,
        finding: Finding::Found(CategoryGroup {
            id: id.into(),
            label: label.into(),
            risk,
            total_size: items.iter().map(|i| i.size).sum(),
            items,
        }),
    }
}

pub fn not_found(id: &str, label: &str, risk: RiskLevel, finding: Finding) -> Category {
    Category {
        id: id.into(),
        label: label.into(),
        risk,
        reason: String::new(),
        comes_back: None,
        route: Route::Trash,
        finding,
    }
}

const MB: u64 = 1_000_000;

pub fn derived_data() -> Category {
    let base = "Library/Developer/Xcode/DerivedData";
    let items = [
        (home(&format!("{base}/TinySift-fmzkqpelrbdvxn")), 3_000 * MB),
        (home(&format!("{base}/ModuleCache.noindex")), 2_000 * MB),
    ]
    .into_iter()
    .chain((0..40).map(|i| (home(&format!("{base}/Build-{i}")), 29 * MB)))
    .chain(std::iter::once((home(&format!("{base}/Logs")), 40 * MB)))
    .collect();
    found(
        "xcode-derived",
        "Xcode DerivedData",
        RiskLevel::Safe,
        Route::Trash,
        ComesBack::Rebuild {
            command: "Build in Xcode",
        },
        "Xcode rebuilds it",
        items,
    )
}

pub const PROJECTS: [(&str, u64); 12] = [
    ("old-landing", 820),
    ("client-portal-2025", 610),
    ("rn-starter", 480),
    ("docs-site", 410),
    ("invoice-tool", 330),
    ("playground/vite-ssr", 290),
    ("storybook-kit", 240),
    ("chrome-ext-notes", 200),
    ("api-mock", 160),
    ("slides", 120),
    ("eslint-config", 90),
    ("tiny-demo", 50),
];

pub fn node_modules() -> Category {
    let mut category = found(
        "node-modules",
        "node_modules (idle)",
        RiskLevel::Review,
        Route::Trash,
        ComesBack::Rebuild {
            command: "npm, pnpm or yarn install",
        },
        "12 projects idle 30+ days",
        PROJECTS
            .iter()
            .map(|(name, mb)| (home(&format!("Code/{name}/node_modules")), mb * MB))
            .collect(),
    );
    if let Finding::Found(group) = &mut category.finding {
        for item in &mut group.items {
            item.evidence = vec![
                Evidence::LastCommit {
                    at: READ_AT - 214 * DAY,
                },
                Evidence::ManifestModified {
                    file: "package.json".into(),
                    at: READ_AT - 231 * DAY,
                },
                Evidence::WorkTreeClean,
            ];
        }
    }
    category
}

fn crash_reports() -> Category {
    let dir = "Library/Logs/DiagnosticReports";
    let items = [
        home(&format!("{dir}/osascript-2026-11-23-111112.ips")),
        home(&format!("{dir}/Retired")),
    ]
    .into_iter()
    .chain((2..89).map(|i| home(&format!("{dir}/report-{i}.diag"))))
    .map(|path| (path, MB))
    .collect();
    found(
        "crash-reports",
        "Crash reports / DiagnosticReports",
        RiskLevel::Safe,
        Route::Trash,
        ComesBack::AppRecreates,
        "macOS writes new reports",
        items,
    )
}

fn user_logs() -> Category {
    let items = [
        (home("Library/Logs/CrashReporter"), 10_000),
        (home("Library/Logs/PhotosSearch.aapbz"), 5_000),
    ]
    .into_iter()
    .chain((0..7).map(|i| (home(&format!("Library/Logs/app-{i}.log")), 5_000)))
    .collect();
    found(
        "user-logs",
        "User logs",
        RiskLevel::Safe,
        Route::Trash,
        ComesBack::AppRecreates,
        "apps write new logs",
        items,
    )
}

fn one(id: &str, label: &str, route: Route, why: &str, path: String, mb: u64) -> Category {
    found(
        id,
        label,
        RiskLevel::Review,
        route,
        ComesBack::Redownload,
        why,
        vec![(path, mb * MB)],
    )
}

/// `DATA.normal.cats`, in the mock's order.
pub fn normal() -> Vec<Category> {
    vec![
        derived_data(),
        node_modules(),
        one(
            "npm",
            "npm cache",
            Route::Trash,
            "npm fetches packages again",
            home(".npm/_cacache"),
            1_100,
        ),
        one(
            "pnpm",
            "pnpm store",
            Route::ForGood,
            "entries nothing references",
            home("Library/pnpm/store"),
            840,
        ),
        one(
            "homebrew",
            "Homebrew cleanup",
            Route::ForGood,
            "old versions, downloads 30+ days old",
            "<homebrew:cleanup>".into(),
            310,
        ),
        crash_reports(),
        user_logs(),
    ]
}

/// `DATA.worst.cats`: a report-only category, an app that runs, sizes from 0 B to 812 GB.
pub fn worst() -> Vec<Category> {
    let more = [
        "Cargo cache",
        "Old screenshots",
        "Mail attachments",
        "Browser caches",
        "User caches",
        "Xcode Archives",
        "Xcode iOS DeviceSupport",
        "Simulator Devices",
        "Old Codex releases",
        "Gradle caches",
        "JetBrains caches",
        "VS Code caches",
        "Go build cache",
        "uv cache",
        "Yarn cache",
        "Old downloads",
        "Streaming caches",
    ];
    let node = found(
        "node-modules",
        "node_modules (idle)",
        RiskLevel::Review,
        Route::Trash,
        ComesBack::Rebuild {
            command: "npm, pnpm or yarn install",
        },
        "1,284 projects idle 30+ days",
        (0..1284)
            .map(|i| {
                let path = format!(
                    "~/Developer/clients/a-very-long-organisation-name/monorepo-{}/packages/experimental/web/node_modules",
                    i + 1
                );
                (path, 196_400 * MB / 1284 + u64::from(i < 196_400 * MB % 1284))
            })
            .collect(),
    );
    [
        found(
            "simulator-runtime-images",
            "Simulator runtime images of every installed Xcode beta and release",
            RiskLevel::Review,
            Route::ReportOnly,
            ComesBack::NotRecoverable,
            "report only: remove it in System Settings › General › Storage",
            (0..14)
                .map(|i| {
                    (
                        format!("/Library/Developer/CoreSimulator/Images/{i}.dmg"),
                        58_000 * MB,
                    )
                })
                .collect(),
        ),
        node,
        not_found(
            "xcode-derived",
            "Xcode DerivedData",
            RiskLevel::Safe,
            Finding::AppRunning {
                app: "Xcode".into(),
            },
        ),
        found(
            "docker",
            "Docker images/build cache",
            RiskLevel::Review,
            Route::ForGood,
            ComesBack::Redownload,
            "unused images and build cache",
            vec![
                ("<docker:images>".into(), 3_400 * MB),
                ("<docker:build-cache>".into(), 2_000 * MB),
            ],
        ),
        found(
            "crash-reports",
            "Crash reports / DiagnosticReports",
            RiskLevel::Safe,
            Route::Trash,
            ComesBack::AppRecreates,
            "macOS writes new reports",
            vec![(home("Library/Logs/DiagnosticReports/a.ips"), 400)],
        ),
        found(
            "user-logs",
            "User logs",
            RiskLevel::Safe,
            Route::Trash,
            ComesBack::AppRecreates,
            "apps write new logs",
            vec![(home("Library/Logs/empty.log"), 0)],
        ),
    ]
    .into_iter()
    .chain(more.iter().enumerate().map(|(i, label)| {
        one(
            &format!("more-{i}"),
            label,
            Route::Trash,
            "worth a look first",
            home(&format!("more/{i}")),
            (17 - i as u64) * 11,
        )
    }))
    .collect()
}
