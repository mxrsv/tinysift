//! Whether a project folder is still in use: its git activity alongside
//! the manifest mtime. Shared by the idle providers (`node-modules`,
//! `rust-targets`, `python-caches`).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::clean::scan_context::ScanContext;
use crate::clean::types::Evidence;
use crate::engine_error;
use crate::error::Result;
use crate::runner::{CommandError, CommandRunner, TOOL_TIMEOUT};

/// Absolute on purpose: a Finder-launched app has a minimal `PATH`.
const GIT: &str = "/usr/bin/git";
const SECS_PER_DAY: i64 = 86_400;

/// Evidence for a project whose manifest already passed the idle filter, or
/// `None` when git says the project is still active or cannot say. Inside a
/// git work tree the last commit touching the project must be older than
/// `idle_days` and the project must have no uncommitted changes. `Err` only
/// when git cannot run at all for a project that needs it.
pub(crate) fn assess(
    runner: &dyn CommandRunner,
    manifest: &Path,
    idle_days: u64,
    ctx: &ScanContext<'_>,
) -> Result<Option<Vec<Evidence>>> {
    if ctx.is_cancelled() {
        return Ok(None);
    }
    let (Some(project), Some(file)) = (manifest.parent(), manifest.file_name()) else {
        return Ok(None);
    };
    let Some(modified) = unix_secs(manifest) else {
        return Ok(None);
    };
    let mut evidence = vec![Evidence::ManifestModified {
        file: file.to_string_lossy().into_owned(),
        at: modified,
    }];
    if !in_git_work_tree(project) {
        evidence.push(Evidence::NotGitRepo);
        return Ok(Some(evidence));
    }
    let Some(project_arg) = project.to_str() else {
        return Ok(None);
    };
    let Some(commit) = last_commit(runner, project_arg)? else {
        return Ok(None);
    };
    if ctx.is_cancelled() || now_secs() - commit <= idle_days as i64 * SECS_PER_DAY {
        return Ok(None);
    }
    if !work_tree_clean(runner, project_arg)? {
        return Ok(None);
    }
    evidence.push(Evidence::LastCommit { at: commit });
    evidence.push(Evidence::WorkTreeClean);
    Ok(Some(evidence))
}

/// Looks for `.git` (a directory, or a file for linked work trees) in
/// `project` and its ancestors without running git.
fn in_git_work_tree(project: &Path) -> bool {
    project
        .ancestors()
        .any(|dir| std::fs::symlink_metadata(dir.join(".git")).is_ok())
}

/// Unix time of the last commit touching `project`; `None` when git cannot
/// answer or no commit touches it.
fn last_commit(runner: &dyn CommandRunner, project: &str) -> Result<Option<i64>> {
    let out = git(runner, project, &["log", "-1", "--format=%ct", "--", "."])?;
    Ok(out.and_then(|text| text.trim().parse().ok()))
}

/// False when there are uncommitted changes or git cannot answer.
fn work_tree_clean(runner: &dyn CommandRunner, project: &str) -> Result<bool> {
    let out = git(runner, project, &["status", "--porcelain", "--", "."])?;
    Ok(out.is_some_and(|text| text.trim().is_empty()))
}

/// Stdout of a successful git call. A non-zero exit, timeout or non-UTF-8
/// output is `None`; only a git that cannot be started is an error.
fn git(runner: &dyn CommandRunner, project: &str, args: &[&str]) -> Result<Option<String>> {
    // --no-optional-locks: status must not write the index of a repo we only inspect.
    let mut full = vec!["--no-optional-locks", "-C", project];
    full.extend_from_slice(args);
    match runner.output(GIT, &full, TOOL_TIMEOUT) {
        Ok(outcome) if outcome.success() => Ok(Some(outcome.stdout)),
        Ok(_) | Err(CommandError::Timeout { .. } | CommandError::NonUtf8(_)) => Ok(None),
        Err(CommandError::NotFound(_) | CommandError::Spawn { .. }) => Err(engine_error!(
            "git is unavailable, so projects inside git repositories cannot be checked"
        )),
    }
}

fn unix_secs(path: &Path) -> Option<i64> {
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;
    use std::fs;
    use std::path::PathBuf;

    use crate::clean::discover::discover_checked;
    use crate::clean::process::test_support::MockChecker;
    use crate::clean::providers::{
        node_modules::NodeModules, python_caches::PythonCaches, rust_targets::RustTargets,
        CleanProvider,
    };
    use crate::clean::scan_context::ScanContext;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    const IDLE_DAYS: u64 = 30;

    struct CancellingRunner {
        flag: AtomicBool,
        calls: AtomicUsize,
    }
    impl CommandRunner for CancellingRunner {
        fn which(&self, _: &str) -> bool {
            true
        }
        fn run(&self, _: &str, _: &[&str]) -> crate::runner::CommandOutput {
            panic!("unbounded runner must not be used")
        }
        fn output(
            &self,
            _: &str,
            _: &[&str],
            _: std::time::Duration,
        ) -> std::result::Result<crate::runner::CommandOutcome, CommandError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.flag.store(true, Ordering::Release);
            Ok(crate::runner::CommandOutcome {
                status: Some(0),
                stdout: "1".into(),
                stderr: String::new(),
            })
        }
    }

    fn cancellation_fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for index in 0..3 {
            let project = root.path().join(format!("project-{index}"));
            for dir in [".git", "node_modules", "target", ".venv"] {
                fs::create_dir_all(project.join(dir)).unwrap();
            }
            fs::write(project.join(".venv/pyvenv.cfg"), "home = fixture").unwrap();
            for manifest in ["package.json", "Cargo.toml", "pyproject.toml"] {
                let file = fs::File::create(project.join(manifest)).unwrap();
                file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1))
                    .unwrap();
            }
        }
        root
    }

    fn cancelling_provider(
        kind: usize,
        root: &Path,
        runner: Arc<CancellingRunner>,
    ) -> Box<dyn CleanProvider> {
        match kind {
            0 => {
                let mut p = NodeModules::with_runner(IDLE_DAYS, runner.clone());
                p.search_roots = vec![root.into()];
                Box::new(p)
            }
            1 => {
                let mut p = RustTargets::with_runner(IDLE_DAYS, runner.clone());
                p.search_roots = vec![root.into()];
                Box::new(p)
            }
            _ => {
                let mut p = PythonCaches::with_runner(IDLE_DAYS, runner.clone());
                p.search_roots = vec![root.into()];
                Box::new(p)
            }
        }
    }

    #[test]
    fn idle_providers_stop_git_requests_after_cancellation() {
        let root = cancellation_fixture();
        for kind in 0..3 {
            let runner = Arc::new(CancellingRunner {
                flag: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
            });
            let providers = vec![cancelling_provider(kind, root.path(), runner.clone())];
            let probe = MockChecker::none();
            let ctx = ScanContext::new(Some(&runner.flag), &probe);
            let report = discover_checked(&providers, &ctx, None);
            assert!(report.cancelled);
            assert_eq!(
                runner.calls.load(Ordering::SeqCst),
                1,
                "{} must not start status or another project's log after cancellation",
                providers[0].id()
            );
        }
    }

    fn tempdir(label: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "tiny-clean-activity-{}-{}",
            label,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    /// `<root>/proj/Cargo.toml`, 90 days old.
    fn old_manifest(root: &Path) -> PathBuf {
        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        let manifest = proj.join("Cargo.toml");
        fs::write(&manifest, b"[package]\n").unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
        fs::File::open(&manifest)
            .unwrap()
            .set_modified(old)
            .unwrap();
        manifest
    }

    fn git_args(project: &Path, rest: &[&str]) -> Vec<String> {
        let mut args = vec!["--no-optional-locks".to_string(), "-C".to_string()];
        args.push(project.to_str().unwrap().to_string());
        args.extend(rest.iter().map(|s| s.to_string()));
        args
    }

    /// Mock git answering `log` with `commit_secs_ago` and `status` with `dirty`.
    fn mock_git(project: &Path, commit_days_ago: i64, status: &str) -> MockRunner {
        let log = git_args(project, &["log", "-1", "--format=%ct", "--", "."]);
        let st = git_args(project, &["status", "--porcelain", "--", "."]);
        let ct = now_secs() - commit_days_ago * SECS_PER_DAY;
        MockRunner::new()
            .with_exit(GIT, &as_strs(&log), 0, &format!("{ct}\n"))
            .with_exit(GIT, &as_strs(&st), 0, status)
    }

    fn as_strs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    fn mark_repo(root: &Path) {
        fs::create_dir_all(root.join(".git")).unwrap();
    }

    fn run(runner: &MockRunner, manifest: &Path) -> Result<Option<Vec<Evidence>>> {
        assess(runner, manifest, IDLE_DAYS, &ScanContext::unchecked())
    }

    #[test]
    fn no_repo_is_decided_by_manifest_and_says_so() {
        let root = tempdir("norepo");
        let manifest = old_manifest(&root);
        let runner = MockRunner::new();
        let evidence = run(&runner, &manifest).unwrap().expect("offered");
        assert!(matches!(
            evidence.as_slice(),
            [Evidence::ManifestModified { file, .. }, Evidence::NotGitRepo] if file == "Cargo.toml"
        ));
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn old_commit_and_clean_tree_is_offered_with_git_evidence() {
        let root = tempdir("clean");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let runner = mock_git(manifest.parent().unwrap(), 60, "");
        let evidence = run(&runner, &manifest).unwrap().expect("offered");
        assert!(matches!(evidence[0], Evidence::ManifestModified { .. }));
        assert!(matches!(evidence[1], Evidence::LastCommit { .. }));
        assert_eq!(evidence[2], Evidence::WorkTreeClean);
        assert_eq!(evidence.len(), 3);
    }

    #[test]
    fn recent_commit_is_not_offered() {
        let root = tempdir("recent");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let runner = mock_git(manifest.parent().unwrap(), 2, "");
        assert_eq!(run(&runner, &manifest).unwrap(), None);
    }

    #[test]
    fn dirty_tree_is_not_offered() {
        let root = tempdir("dirty");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let runner = mock_git(manifest.parent().unwrap(), 60, " M src/main.rs\n");
        assert_eq!(run(&runner, &manifest).unwrap(), None);
    }

    #[test]
    fn git_non_zero_exit_is_not_offered() {
        let root = tempdir("nonzero");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let log = git_args(
            manifest.parent().unwrap(),
            &["log", "-1", "--format=%ct", "--", "."],
        );
        let old = format!("{}\n", now_secs() - 60 * SECS_PER_DAY);
        let runner = MockRunner::new().with_exit(GIT, &as_strs(&log), 128, &old);
        assert_eq!(run(&runner, &manifest).unwrap(), None);
    }

    #[test]
    fn git_status_failure_is_not_offered() {
        let root = tempdir("status-fails");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let project = manifest.parent().unwrap();
        let st = git_args(project, &["status", "--porcelain", "--", "."]);
        let runner = mock_git(project, 60, "").with_exit(GIT, &as_strs(&st), 128, "");
        assert_eq!(run(&runner, &manifest).unwrap(), None);
    }

    #[test]
    fn git_timeout_is_not_offered() {
        let root = tempdir("timeout");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let log = git_args(
            manifest.parent().unwrap(),
            &["log", "-1", "--format=%ct", "--", "."],
        );
        let timeout = CommandError::Timeout {
            bin: GIT.to_string(),
            timeout_ms: 30_000,
        };
        let runner = MockRunner::new().with_output(GIT, &as_strs(&log), Err(timeout));
        assert_eq!(run(&runner, &manifest).unwrap(), None);
    }

    #[test]
    fn unparsable_commit_time_is_not_offered() {
        let root = tempdir("garbage");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let log = git_args(
            manifest.parent().unwrap(),
            &["log", "-1", "--format=%ct", "--", "."],
        );
        let runner = MockRunner::new().with_exit(GIT, &as_strs(&log), 0, "not a number\n");
        assert_eq!(run(&runner, &manifest).unwrap(), None);
    }

    #[test]
    fn unavailable_git_inside_a_repo_is_an_error() {
        let root = tempdir("nogit");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let err = run(&MockRunner::new(), &manifest).unwrap_err();
        assert!(err.to_string().contains("git is unavailable"), "{err}");
    }

    #[test]
    fn git_file_marks_a_linked_work_tree() {
        let root = tempdir("worktree");
        fs::write(root.join(".git"), b"gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        let manifest = old_manifest(&root);
        let runner = mock_git(manifest.parent().unwrap(), 2, "");
        // Recent commit proves git was consulted, so the .git file was detected.
        assert_eq!(run(&runner, &manifest).unwrap(), None);
        assert!(!runner.output_calls.lock().unwrap().is_empty());
    }
}
