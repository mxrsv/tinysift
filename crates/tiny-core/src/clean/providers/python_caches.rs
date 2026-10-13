use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

use super::project_activity::assess;
use super::{dev_search_roots, execute_per_item, is_idle, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, walk_with};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};
use crate::runner::{CommandRunner, RealRunner};

const ID: &str = "python-caches";
const LABEL: &str = "Python __pycache__ / venv (idle)";

const VENV_NAMES: &[&str] = &["venv", ".venv", "env"];

pub struct PythonCaches {
    pub idle_days: u64,
    pub search_roots: Vec<PathBuf>,
    runner: Arc<dyn CommandRunner>,
}

impl PythonCaches {
    pub fn new(idle_days: u64) -> Self {
        Self::with_runner(idle_days, Arc::new(RealRunner))
    }

    pub fn with_runner(idle_days: u64, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            idle_days,
            search_roots: dev_search_roots(),
            runner,
        }
    }
}

impl CleanProvider for PythonCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        format!(
            "__pycache__ folders, and virtualenvs in projects with no edits, commits or uncommitted changes for {} days",
            self.idle_days
        )
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
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let mut items = Vec::new();
        for root in &self.search_roots {
            for found in find_pycache(ctx, root) {
                let size = dir_size_checked(&found, ctx);
                items.push(CleanItem {
                    category_id: ID.to_string(),
                    category_label: LABEL.to_string(),
                    path: found,
                    size,
                    risk: RiskLevel::Review,
                    evidence: Vec::new(),
                });
            }
            for found in find_orphan_venv(ctx, root, self.idle_days) {
                if ctx.is_cancelled() {
                    return Ok(Vec::new());
                }
                let Some(manifest) = found.parent().and_then(python_manifest) else {
                    continue;
                };
                let Some(mut evidence) = assess(&*self.runner, &manifest, self.idle_days, ctx)?
                else {
                    continue;
                };
                evidence.push(Evidence::VenvMarker);
                let size = dir_size_checked(&found, ctx);
                items.push(CleanItem {
                    category_id: ID.to_string(),
                    category_label: LABEL.to_string(),
                    path: found,
                    size,
                    risk: RiskLevel::Review,
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

/// Walks `root` symlink-safe and returns every `__pycache__` dir. Manifest
/// check NOT required — pycache is always safe to delete. Virtualenvs are
/// not descended: their caches go with the venv, not as separate items.
pub fn find_pycache(ctx: &ScanContext<'_>, root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    walk_with(root, ctx, |path, meta| {
        if !meta.file_type().is_dir() || is_named_venv(path) {
            return false;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("__pycache__") {
            found.push(path.to_path_buf());
            return false; // never recurse into pycache
        }
        true
    });
    found
}

/// Walks `root` symlink-safe and returns every venv-style dir that holds a
/// `pyvenv.cfg` and whose parent has a python manifest (pyproject.toml /
/// setup.py / requirements.txt) AND the manifest is idle. The name alone is
/// not enough: a package folder called `env/` is source code.
pub fn find_orphan_venv(ctx: &ScanContext<'_>, root: &Path, idle_days: u64) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    walk_with(root, ctx, |path, meta| {
        if !meta.file_type().is_dir() {
            return false;
        }
        if is_named_venv(path) {
            if let Some(parent) = path.parent() {
                if let Some(manifest) = python_manifest(parent) {
                    if is_idle(&manifest, idle_days) {
                        found.push(path.to_path_buf());
                    }
                }
            }
            return false; // never recurse into venv
        }
        true
    });
    found
}

/// True when `dir` has a venv name and is a virtualenv: `python -m venv`
/// and virtualenv both write `pyvenv.cfg` at its top level.
fn is_named_venv(dir: &Path) -> bool {
    let named =
        matches!(dir.file_name().and_then(|n| n.to_str()), Some(n) if VENV_NAMES.contains(&n));
    named
        && std::fs::symlink_metadata(dir.join("pyvenv.cfg"))
            .map(|m| m.file_type().is_file())
            .unwrap_or(false)
}

/// Returns the first existing python manifest (pyproject.toml / setup.py /
/// requirements.txt) under `dir`, or None.
pub fn python_manifest(dir: &Path) -> Option<PathBuf> {
    for name in ["pyproject.toml", "setup.py", "requirements.txt"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-py-{}-{}",
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
    fn python_caches_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn pycache_found_anywhere_no_manifest_required() {
        let root = tempdir("pyc");
        let nested = root.join("a/b/__pycache__");
        fs::create_dir_all(&nested).unwrap();
        let found = find_pycache(&ScanContext::unchecked(), &root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], nested);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    fn make_venv(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("pyvenv.cfg"), b"home = /usr/bin\n").unwrap();
    }

    #[test]
    fn source_folder_named_env_is_not_a_venv() {
        let root = tempdir("srcenv");
        let proj = root.join("app");
        fs::create_dir_all(proj.join("env")).unwrap();
        fs::write(proj.join("env/settings.py"), b"DEBUG = False\n").unwrap();
        let manifest = proj.join("requirements.txt");
        fs::write(&manifest, b"requests\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
        std::fs::File::open(&manifest)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(find_orphan_venv(&ScanContext::unchecked(), &root, 30).is_empty());
        make_venv(&proj.join("venv"));
        let found = find_orphan_venv(&ScanContext::unchecked(), &root, 30);
        assert_eq!(found, vec![proj.join("venv")]);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn pycache_inside_a_venv_is_not_listed_separately() {
        let root = tempdir("venvpyc");
        let venv = root.join("p/.venv");
        make_venv(&venv);
        fs::create_dir_all(venv.join("lib/python3.12/site-packages/x/__pycache__")).unwrap();
        fs::create_dir_all(root.join("p/src/__pycache__")).unwrap();
        let found = find_pycache(&ScanContext::unchecked(), &root);
        assert_eq!(found, vec![root.join("p/src/__pycache__")]);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn venv_requires_python_manifest() {
        let root = tempdir("orphanvenv");
        let proj = root.join("ghost");
        make_venv(&proj.join("venv"));
        // No manifest → must not be flagged.
        let found = find_orphan_venv(&ScanContext::unchecked(), &root, 0);
        assert!(found.is_empty());
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn venv_idle_threshold_applied() {
        let root = tempdir("freshvenv");
        let proj = root.join("alive");
        make_venv(&proj.join(".venv"));
        let manifest = proj.join("pyproject.toml");
        fs::write(&manifest, b"[project]\nname=\"x\"\n").unwrap();
        let fresh = find_orphan_venv(&ScanContext::unchecked(), &root, 30);
        assert!(fresh.is_empty(), "fresh manifest must not flag");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 86_400);
        std::fs::File::open(&manifest)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let stale = find_orphan_venv(&ScanContext::unchecked(), &root, 30);
        assert_eq!(stale.len(), 1);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    use crate::runner::test_support::MockRunner;

    fn provider(root: &Path, runner: MockRunner) -> PythonCaches {
        let mut p = PythonCaches::with_runner(30, Arc::new(runner));
        p.search_roots = vec![root.to_path_buf()];
        p
    }

    #[test]
    fn discover_marks_venv_and_leaves_pycache_without_evidence() {
        let root = tempdir("discover");
        let proj = root.join("p");
        make_venv(&proj.join(".venv"));
        fs::create_dir_all(proj.join("src/__pycache__")).unwrap();
        let manifest = proj.join("pyproject.toml");
        fs::write(&manifest, b"[project]\nname=\"x\"\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
        std::fs::File::open(&manifest)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let items = provider(&root, MockRunner::new())
            .discover(&ScanContext::unchecked())
            .unwrap();
        assert_eq!(items.len(), 2);
        let venv = items.iter().find(|i| i.path.ends_with(".venv")).unwrap();
        assert_eq!(venv.evidence.last(), Some(&Evidence::VenvMarker));
        assert!(venv.evidence.contains(&Evidence::NotGitRepo));
        let pyc = items
            .iter()
            .find(|i| i.path.ends_with("__pycache__"))
            .unwrap();
        assert!(pyc.evidence.is_empty());
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }
}
