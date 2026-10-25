//! Detect leftover `~/Library/Application Support/<bundle-id>` directories
//! whose owning app has been uninstalled.
//!
//! Approach: build a set of installed bundle IDs via Spotlight
//! (`mdfind -attr kMDItemCFBundleIdentifier` over every application
//! bundle), then flag any direct child of `Application Support/` whose name doesn't
//! match. Risk is **always Review** — false positives are inevitable
//! because some apps store their data under a different key than their
//! bundle ID.
//!
//! **Report only.** Folder names such as `AddressBook` or `CloudDocs`
//! match no bundle id yet belong to macOS or a running app, so `execute`
//! refuses every item until ownership is checked per bundle id.
//!
//! Spotlight tắt → `mdfind` returns 0 lines → we MUST refuse to discover
//! (otherwise every dir would look orphan). The handoff calls this out
//! explicitly: "if mdfind returns 0 result, provider refuse discover".

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::{run_tool, top_level_entries, CleanProvider, ReportOnly};
use crate::clean::runner::{CommandRunner, RealRunner};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "app-orphans";
const LABEL: &str = "App leftovers (report only)";
const REPORT_ONLY: &str =
    "report only: a folder name does not prove its app is gone; check it in Finder";

const MDFIND_QUERY: &str = "kMDItemContentType == 'com.apple.application-bundle'";
const BUNDLE_ID_ATTR: &str = "kMDItemCFBundleIdentifier";
/// `mdfind -attr` prints `<path><padding>kMDItemCFBundleIdentifier = <value>`.
const ATTR_SEPARATOR: &str = " kMDItemCFBundleIdentifier = ";
/// What `mdfind -attr` prints when Spotlight has no value for the attribute.
const ATTR_NULL: &str = "(null)";
const APP_SUPPORT: &str = "Library/Application Support";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct AppOrphans {
    runner: Arc<dyn CommandRunner>,
}

impl AppOrphans {
    pub fn new() -> Self {
        Self {
            runner: Arc::new(RealRunner),
        }
    }
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Builds the set of `CFBundleIdentifier` values for every `.app`
    /// bundle Spotlight knows about. Empty result → caller must refuse
    /// to flag anything.
    /// One `mdfind -attr` call returns every ID; only apps Spotlight has no
    /// ID for fall back to a `defaults read` each. A tool that cannot run
    /// (or times out) is an error: skipping one app would make its support
    /// dir look orphaned.
    fn installed_bundle_ids(&self) -> Result<HashSet<String>> {
        let out = run_tool(
            self.runner.as_ref(),
            "mdfind",
            &["-attr", BUNDLE_ID_ATTR, MDFIND_QUERY],
        )?;
        if !out.success() {
            return Ok(HashSet::new());
        }
        let mut ids: HashSet<String> = HashSet::new();
        for line in out.stdout.lines() {
            let (app_path, id) = match line.rsplit_once(ATTR_SEPARATOR) {
                Some((path, id)) => (path.trim(), id.trim()),
                None => (line.trim(), ""),
            };
            if app_path.is_empty() {
                continue;
            }
            if !id.is_empty() && id != ATTR_NULL {
                ids.insert(id.to_string());
            } else if let Some(id) = self.bundle_id_from_plist(app_path)? {
                ids.insert(id);
            }
        }
        Ok(ids)
    }

    /// `defaults read <app>/Contents/Info CFBundleIdentifier` (no .plist
    /// suffix — defaults reads .plist by convention). `None` when the app
    /// has no readable ID.
    fn bundle_id_from_plist(&self, app_path: &str) -> Result<Option<String>> {
        let plist_arg = format!("{}/Contents/Info", app_path);
        let out = run_tool(
            self.runner.as_ref(),
            "defaults",
            &["read", &plist_arg, "CFBundleIdentifier"],
        )?;
        let id = out.stdout.trim();
        Ok((out.success() && !id.is_empty()).then(|| id.to_string()))
    }
}

impl Default for AppOrphans {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps only entries whose dir name doesn't match any installed bundle id.
fn without_installed(candidates: Vec<CleanItem>, installed: &HashSet<String>) -> Vec<CleanItem> {
    candidates
        .into_iter()
        .filter(|item| {
            let name = match item.path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => return false,
            };
            !installed.contains(name)
        })
        .collect()
}

impl CleanProvider for AppOrphans {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Application Support folders whose name matches no installed app's bundle ID".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_report_only_reason(&self) -> ReportOnly {
        ReportOnly::UnreliableMatch
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let installed = self.installed_bundle_ids()?;
        if installed.is_empty() {
            // Spotlight off / not indexed yet → refuse rather than flag everything.

            return Ok(Vec::new());
        }
        let support_dir = h.join(APP_SUPPORT);
        let candidates = top_level_entries(ctx, &support_dir, ID, LABEL, RiskLevel::Review);
        Ok(without_installed(candidates, &installed))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use crate::clean::runner::test_support::MockRunner;

    #[test]
    fn app_orphans_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn app_orphans_review_risk() {
        assert_eq!(AppOrphans::new().risk(), RiskLevel::Review);
    }

    #[test]
    fn execute_refuses_every_item_and_leaves_it_in_place() {
        let fixture = tempfile::tempdir().unwrap();
        let dir = fixture.path().canonicalize().unwrap();
        std::fs::create_dir_all(dir.join("Data")).unwrap();
        let item = CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path: dir.clone(),
            size: 0,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        };
        let p = AppOrphans::with_runner(Arc::new(MockRunner::new()));
        for action in [ExecAction::Trash, ExecAction::HardDelete] {
            let report = p.execute(std::slice::from_ref(&item), action).unwrap();
            assert!(report.removed_paths.is_empty());
            assert_eq!(report.failed, vec![(dir.clone(), REPORT_ONLY.to_string())]);
            assert!(std::fs::symlink_metadata(dir.join("Data")).is_ok_and(|m| m.is_dir()));
        }
        assert!(p.execute(&[item], ExecAction::EmptyTrash).is_err());
        crate::clean::fs_safe::remove_recursive_safe(&dir).unwrap();
    }

    const ATTR_ARGS: [&str; 3] = ["-attr", BUNDLE_ID_ATTR, MDFIND_QUERY];

    fn defaults_args(app: &str) -> [String; 3] {
        [
            "read".into(),
            format!("{app}/Contents/Info"),
            "CFBundleIdentifier".into(),
        ]
    }

    fn with_defaults(runner: MockRunner, app: &str, status: i32, stdout: &str) -> MockRunner {
        let args = defaults_args(app);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        runner.with_exit("defaults", &args, status, stdout)
    }

    fn item(name: &str) -> CleanItem {
        CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path: PathBuf::from("/support").join(name),
            size: 0,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn empty_mdfind_result_returns_empty_safely() {
        // mdfind returns success but zero lines → installed set empty →
        // discover() must return empty (not flag everything in Application Support).
        let runner = Arc::new(MockRunner::new().with_exit("mdfind", &ATTR_ARGS, 0, ""));
        let p = AppOrphans::with_runner(runner);
        let items = p.discover(&ScanContext::unchecked()).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn mdfind_failure_returns_empty_safely() {
        // mdfind exits non-zero → also empty, and no per-app fallback runs.
        let runner = Arc::new(MockRunner::new().with_exit("mdfind", &ATTR_ARGS, 1, ""));
        let p = AppOrphans::with_runner(runner.clone());
        assert!(p.installed_bundle_ids().unwrap().is_empty());
        assert!(p.discover(&ScanContext::unchecked()).unwrap().is_empty());
        assert!(runner
            .output_calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.starts_with("mdfind ")));
    }

    #[test]
    fn mdfind_that_cannot_run_is_an_error() {
        let p = AppOrphans::with_runner(Arc::new(MockRunner::new()));
        assert!(p.installed_bundle_ids().is_err());
    }

    #[test]
    fn installed_bundle_ids_reads_every_id_from_one_mdfind_call() {
        let runner = Arc::new(MockRunner::new().with_exit(
            "mdfind",
            &ATTR_ARGS,
            0,
            "/Applications/Safari.app   kMDItemCFBundleIdentifier = com.apple.Safari\n\
             /Applications/Music.app   kMDItemCFBundleIdentifier = com.apple.Music\n",
        ));
        let p = AppOrphans::with_runner(runner.clone());
        let ids = p.installed_bundle_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("com.apple.Safari"));
        assert!(ids.contains("com.apple.Music"));
        // No `defaults` subprocess: that is the point of `-attr`.
        assert_eq!(runner.output_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn installed_bundle_ids_keeps_spaces_in_app_path() {
        let runner = Arc::new(MockRunner::new().with_exit(
            "mdfind",
            &ATTR_ARGS,
            0,
            "/System/Applications/Utilities/Activity Monitor.app   kMDItemCFBundleIdentifier = com.apple.ActivityMonitor\n",
        ));
        let ids = AppOrphans::with_runner(runner)
            .installed_bundle_ids()
            .unwrap();
        assert_eq!(
            ids,
            HashSet::from(["com.apple.ActivityMonitor".to_string()])
        );
    }

    #[test]
    fn installed_bundle_ids_splits_on_the_last_separator() {
        // A path that itself contains the attribute text must not confuse the split.
        let app = "/Apps/odd kMDItemCFBundleIdentifier = x.app";
        let runner = Arc::new(MockRunner::new().with_exit(
            "mdfind",
            &ATTR_ARGS,
            0,
            &format!("{app}   kMDItemCFBundleIdentifier = com.example.odd\n"),
        ));
        let ids = AppOrphans::with_runner(runner)
            .installed_bundle_ids()
            .unwrap();
        assert_eq!(ids, HashSet::from(["com.example.odd".to_string()]));
    }

    #[test]
    fn installed_bundle_ids_falls_back_to_defaults_for_null() {
        let app = "/Applications/No Spotlight.app";
        let runner = MockRunner::new().with_exit(
            "mdfind",
            &ATTR_ARGS,
            0,
            "/Applications/Good.app   kMDItemCFBundleIdentifier = com.example.good\n\
             /Applications/No Spotlight.app   kMDItemCFBundleIdentifier = (null)\n",
        );
        let runner = Arc::new(with_defaults(runner, app, 0, "com.example.plist\n"));
        let ids = AppOrphans::with_runner(runner.clone())
            .installed_bundle_ids()
            .unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("com.example.good"));
        assert!(ids.contains("com.example.plist"));
        // Only the `(null)` app costs a subprocess.
        assert_eq!(runner.output_calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn installed_bundle_ids_skips_null_apps_with_failed_defaults() {
        let app = "/Applications/Bad.app";
        let runner = MockRunner::new().with_exit(
            "mdfind",
            &ATTR_ARGS,
            0,
            "/Applications/Good.app   kMDItemCFBundleIdentifier = com.example.good\n\
             /Applications/Bad.app   kMDItemCFBundleIdentifier = (null)\n",
        );
        let runner = Arc::new(with_defaults(runner, app, 1, ""));
        let ids = AppOrphans::with_runner(runner)
            .installed_bundle_ids()
            .unwrap();
        assert_eq!(ids, HashSet::from(["com.example.good".to_string()]));
    }

    #[test]
    fn installed_bundle_ids_tolerates_empty_and_odd_lines() {
        // Blank lines are ignored; a line without the attribute is treated as a
        // bare app path and read through `defaults`; an empty value does the same.
        let runner = MockRunner::new().with_exit(
            "mdfind",
            &ATTR_ARGS,
            0,
            "\n   \n/Applications/Bare.app\n/Applications/Blank.app   kMDItemCFBundleIdentifier = \n",
        );
        let runner = with_defaults(runner, "/Applications/Bare.app", 0, "com.example.bare\n");
        let runner = Arc::new(with_defaults(runner, "/Applications/Blank.app", 1, ""));
        let ids = AppOrphans::with_runner(runner)
            .installed_bundle_ids()
            .unwrap();
        assert_eq!(ids, HashSet::from(["com.example.bare".to_string()]));
    }

    #[test]
    fn without_installed_keeps_only_unmatched_folders() {
        let installed = HashSet::from(["com.apple.Safari".to_string()]);
        let kept = without_installed(
            vec![
                item("com.apple.Safari"),
                item("com.gone.App"),
                item("Slack"),
            ],
            &installed,
        );
        let names: Vec<_> = kept
            .iter()
            .map(|i| i.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["com.gone.App", "Slack"]);
    }
}
