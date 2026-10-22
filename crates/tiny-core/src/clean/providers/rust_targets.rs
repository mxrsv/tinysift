use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

use super::project_activity::{assess_build_output, refuse_tracked, BUILD_OUTPUT_SAFE_DAYS};
use super::{dev_search_roots, execute_per_item, is_idle, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, walk_with};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::options::CleanOptions;
use crate::runner::{CommandRunner, RealRunner};

const ID: &str = "rust-targets";
const LABEL: &str = "Rust target/ (idle)";
/// The desktop lists every project, idle or not.
const ALL_PROJECTS_LABEL: &str = "Rust target/";

pub struct RustTargets {
    pub idle_days: u64,
    /// List every project's `target/` (the desktop), not only idle ones.
    pub all_projects: bool,
    pub search_roots: Vec<PathBuf>,
    runner: Arc<dyn CommandRunner>,
}

impl RustTargets {
    pub fn new(idle_days: u64) -> Self {
        Self::with_runner(idle_days, Arc::new(RealRunner))
    }

    pub fn with_runner(idle_days: u64, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            idle_days,
            all_projects: false,
            search_roots: dev_search_roots(),
            runner,
        }
    }

    /// The provider `opts` asks for: every project when `all_build_output`.
    pub fn with_options(opts: &CleanOptions, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            all_projects: opts.all_build_output,
            ..Self::with_runner(opts.idle_days, runner)
        }
    }

    /// `None` lists every project.
    fn idle_filter(&self) -> Option<u64> {
        (!self.all_projects).then_some(self.idle_days)
    }
}

impl CleanProvider for RustTargets {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        if self.all_projects {
            ALL_PROJECTS_LABEL
        } else {
            LABEL
        }
    }
    fn inclusion_reason(&self) -> String {
        if self.all_projects {
            return format!("Rust target/ folders in your projects; cargo build restores them. Safe once a project in git has no edits, commits or uncommitted changes for {BUILD_OUTPUT_SAFE_DAYS} days");
        }
        format!("Rust target/ folders in projects with no edits, commits or uncommitted changes for {} days; cargo build restores them", self.idle_days)
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        !self.search_roots.is_empty()
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        refuse_tracked(&*self.runner, path)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let mut items = Vec::new();
        for root in &self.search_roots {
            for found in find_rust_targets(ctx, root, self.idle_filter()) {
                if ctx.is_cancelled() {
                    return Ok(Vec::new());
                }
                let manifest = found.with_file_name("Cargo.toml");
                let Some((risk, evidence)) =
                    assess_build_output(&*self.runner, &found, &manifest, self.idle_filter(), ctx)?
                else {
                    continue;
                };
                let size = dir_size_checked(&found, ctx);
                items.push(CleanItem {
                    category_id: ID.to_string(),
                    category_label: self.label().to_string(),
                    path: found,
                    size,
                    risk,
                    evidence,
                });
            }
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Walks `root` symlink-safe and returns every `target` dir whose parent
/// has a `Cargo.toml`, modified more than `idle_days` ago unless that is
/// `None`. Does NOT descend into a `target` dir once found.
pub fn find_rust_targets(
    ctx: &ScanContext<'_>,
    root: &Path,
    idle_days: Option<u64>,
) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    walk_with(root, ctx, |path, meta| {
        if !meta.file_type().is_dir() {
            return false;
        }
        let is_target = path.file_name().and_then(|n| n.to_str()) == Some("target");
        if is_target {
            if let Some(parent) = path.parent() {
                let manifest = parent.join("Cargo.toml");
                if manifest.is_file() && idle_days.is_none_or(|days| is_idle(&manifest, days)) {
                    found.push(path.to_path_buf());
                }
            }
            // Never recurse into target/.
            return false;
        }
        true
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-rust-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn rust_targets_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn refuses_target_without_cargo_toml() {
        let root = tempdir("orphan");
        let proj = root.join("ghost");
        fs::create_dir_all(proj.join("target")).unwrap();
        let found = find_rust_targets(&ScanContext::unchecked(), &root, Some(0));
        assert!(found.is_empty());
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    fn backdate(p: &Path, days: u64) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        std::fs::File::open(p).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn idle_threshold_filters_recently_modified() {
        let root = tempdir("idle");
        let proj = root.join("alive");
        fs::create_dir_all(proj.join("target")).unwrap();
        let manifest = proj.join("Cargo.toml");
        fs::write(&manifest, b"[package]\nname = \"x\"\n").unwrap();
        let fresh = find_rust_targets(&ScanContext::unchecked(), &root, Some(30));
        assert!(fresh.is_empty(), "fresh Cargo.toml must not flag");
        backdate(&manifest, 31);
        let stale = find_rust_targets(&ScanContext::unchecked(), &root, Some(30));
        assert_eq!(stale.len(), 1);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn does_not_descend_into_target() {
        let root = tempdir("nested");
        let proj = root.join("p");
        let outer = proj.join("target");
        fs::create_dir_all(outer.join("target")).unwrap();
        let manifest = proj.join("Cargo.toml");
        fs::write(&manifest, b"[package]\nname = \"x\"\n").unwrap();
        backdate(&manifest, 40);
        let found = find_rust_targets(&ScanContext::unchecked(), &root, Some(30));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], outer);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    use crate::clean::types::Evidence;
    use crate::runner::test_support::MockRunner;

    fn provider(root: &Path, runner: MockRunner) -> RustTargets {
        let mut p = RustTargets::with_runner(30, Arc::new(runner));
        p.search_roots = vec![root.to_path_buf()];
        p
    }

    fn idle_project(root: &Path) -> PathBuf {
        let proj = root.join("p");
        fs::create_dir_all(proj.join("target")).unwrap();
        let manifest = proj.join("Cargo.toml");
        fs::write(&manifest, b"[package]\nname = \"x\"\n").unwrap();
        backdate(&manifest, 90);
        proj
    }

    #[test]
    fn discover_offers_idle_project_outside_git_with_evidence() {
        let root = tempdir("discover-nogit");
        idle_project(&root);
        let items = provider(&root, MockRunner::new())
            .discover(&ScanContext::unchecked())
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].evidence.last(), Some(&Evidence::NotGitRepo));
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn discover_fails_when_git_is_unavailable_for_a_repo_project() {
        let root = tempdir("discover-nogit-bin");
        fs::create_dir_all(root.join(".git")).unwrap();
        idle_project(&root);
        let err = provider(&root, MockRunner::new())
            .discover(&ScanContext::unchecked())
            .unwrap_err();
        assert!(err.to_string().contains("git is unavailable"), "{err}");
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    /// `<root>/<name>/Cargo.toml`, `days` old, beside `target/`. Returns `target/`.
    fn project(root: &Path, name: &str, days: u64) -> PathBuf {
        let proj = root.join(name);
        fs::create_dir_all(proj.join("target")).unwrap();
        let manifest = proj.join("Cargo.toml");
        fs::write(&manifest, b"[package]\nname = \"x\"\n").unwrap();
        backdate(&manifest, days);
        proj.join("target")
    }

    #[test]
    fn desktop_lists_every_project_and_none_outside_git_is_safe() {
        let root = tempdir("desktop");
        let old = project(&root, "old", 40);
        let idle = project(&root, "idle", 8);
        let active = project(&root, "active", 2);
        let desktop = RustTargets::with_options(
            &crate::options::CleanOptions {
                all_build_output: true,
                ..Default::default()
            },
            Arc::new(MockRunner::new()),
        );
        let desktop = RustTargets {
            search_roots: vec![root.clone()],
            ..desktop
        };
        assert_eq!(desktop.label(), "Rust target/");
        assert!(desktop.inclusion_reason().contains("7 days"));
        let items = desktop.discover(&ScanContext::unchecked()).unwrap();
        let risk = |path: &Path| items.iter().find(|i| i.path == path).map(|i| i.risk);
        // Outside git a manifest's age alone never makes a project Safe.
        assert_eq!(risk(&old), Some(RiskLevel::Review));
        assert_eq!(risk(&idle), Some(RiskLevel::Review));
        assert_eq!(risk(&active), Some(RiskLevel::Review));
        let old_item = items.iter().find(|i| i.path == old).unwrap();
        assert_eq!(old_item.evidence.last(), Some(&Evidence::NotGitRepo));
        assert!(items.iter().all(|i| i.category_label == "Rust target/"));
        // The CLI's scope on the same fixture: idle for 30 days, Review.
        let cli = provider(&root, MockRunner::new())
            .discover(&ScanContext::unchecked())
            .unwrap();
        let listed: Vec<_> = cli.iter().map(|i| (&i.path, i.risk)).collect();
        assert_eq!(listed, vec![(&old, RiskLevel::Review)]);
        assert_eq!(cli[0].category_label, LABEL);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }
}
