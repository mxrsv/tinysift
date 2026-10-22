//! Whether a project folder is still in use: its git activity alongside
//! the manifest mtime. Shared by the idle providers (`node-modules`,
//! `rust-targets`, `python-caches`); the build output ones also never offer
//! a folder holding files git tracks.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::clean::scan_context::ScanContext;
use crate::clean::types::{Evidence, RiskLevel};
use crate::engine_error;
use crate::error::Result;
use crate::runner::{CommandError, CommandRunner, TOOL_TIMEOUT};

/// Absolute on purpose: a Finder-launched app has a minimal `PATH`.
const GIT: &str = "/usr/bin/git";
const SECS_PER_DAY: i64 = 86_400;

/// Days without edits, commits or uncommitted changes after which the
/// desktop's build output is Safe, so the app preselects it.
pub const BUILD_OUTPUT_SAFE_DAYS: u64 = 7;

/// What the manifest and git say about a project.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Activity {
    /// The facts gathered, in order; partial when activity could not be told.
    pub evidence: Vec<Evidence>,
    /// No manifest edit, commit or uncommitted change for more than the
    /// asked days, every fact checked.
    pub idle: bool,
}

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
    let activity = activity(runner, manifest, idle_days, ctx)?;
    Ok(activity.idle.then_some(activity.evidence))
}

/// Whether the project of `manifest` is idle for more than `idle_days`, with
/// the facts behind the answer. Anything that cannot be told (unreadable
/// manifest, git without an answer, cancellation) is not idle. `Err` only
/// when git cannot run at all for a project that needs it.
pub(crate) fn activity(
    runner: &dyn CommandRunner,
    manifest: &Path,
    idle_days: u64,
    ctx: &ScanContext<'_>,
) -> Result<Activity> {
    let active = |evidence| {
        Ok(Activity {
            evidence,
            idle: false,
        })
    };
    if ctx.is_cancelled() {
        return active(Vec::new());
    }
    let (Some(project), Some(file)) = (manifest.parent(), manifest.file_name()) else {
        return active(Vec::new());
    };
    let Some(modified) = unix_secs(manifest) else {
        return active(Vec::new());
    };
    let mut evidence = vec![Evidence::ManifestModified {
        file: file.to_string_lossy().into_owned(),
        at: modified,
    }];
    if !older_than(modified, idle_days) {
        return active(evidence);
    }
    if !in_git_work_tree(project) {
        evidence.push(Evidence::NotGitRepo);
        return Ok(Activity {
            evidence,
            idle: true,
        });
    }
    let Some(project_arg) = project.to_str() else {
        return active(evidence);
    };
    let Some(commit) = last_commit(runner, project_arg)? else {
        return active(evidence);
    };
    evidence.push(Evidence::LastCommit { at: commit });
    if ctx.is_cancelled() || !older_than(commit, idle_days) {
        return active(evidence);
    }
    if !work_tree_clean(runner, project_arg)? {
        return active(evidence);
    }
    evidence.push(Evidence::WorkTreeClean);
    Ok(Activity {
        evidence,
        idle: true,
    })
}

/// Risk and evidence for a project's build output `folder` (`target/`,
/// `node_modules`) beside `manifest`, or `None` when it is not offered.
/// `Some(days)` (the CLI) offers only a project idle for more than `days`,
/// as Review. `None` (the desktop) offers every project: Safe when idle for
/// more than `BUILD_OUTPUT_SAFE_DAYS` with every fact checked, Review when
/// active or when that cannot be told. Either way a folder holding a file
/// git tracks, or whose tracked files git cannot list, is never offered.
pub(crate) fn assess_build_output(
    runner: &dyn CommandRunner,
    folder: &Path,
    manifest: &Path,
    idle_days: Option<u64>,
    ctx: &ScanContext<'_>,
) -> Result<Option<(RiskLevel, Vec<Evidence>)>> {
    let offered = match idle_days {
        Some(days) => assess(runner, manifest, days, ctx)?.map(|e| (RiskLevel::Review, e)),
        None => {
            let Activity { evidence, idle } =
                activity(runner, manifest, BUILD_OUTPUT_SAFE_DAYS, ctx)?;
            let risk = if idle {
                RiskLevel::Safe
            } else {
                RiskLevel::Review
            };
            Some((risk, evidence))
        }
    };
    let Some(offered) = offered else {
        return Ok(None);
    };
    if ctx.is_cancelled() || holds_tracked_files(runner, folder)? {
        return Ok(None);
    }
    Ok(Some(offered))
}

/// True when git tracks a file inside `folder`, or cannot say (fail closed).
/// A folder outside every git work tree holds no tracked file and runs no
/// git. `Err` only when git cannot run at all.
pub(crate) fn holds_tracked_files(runner: &dyn CommandRunner, folder: &Path) -> Result<bool> {
    // The folder's own `.git` counts: a `node_modules` may itself be a repo.
    if !in_git_work_tree(folder) {
        return Ok(false);
    }
    let Some(folder_arg) = folder.to_str() else {
        return Ok(true);
    };
    let listed = git(runner, folder_arg, &["ls-files", "--", "."])?;
    Ok(listed.is_none_or(|files| !files.trim().is_empty()))
}

/// `check_item` for build output: refuses a folder that holds a file git
/// tracks, or whose tracked files git cannot list, right before it is acted
/// on. A folder that cannot be stat'ed passes: the path checks report it.
pub(crate) fn refuse_tracked(
    runner: &dyn CommandRunner,
    folder: &Path,
) -> std::result::Result<(), String> {
    if std::fs::symlink_metadata(folder).is_err() {
        return Ok(());
    }
    match holds_tracked_files(runner, folder) {
        Ok(false) => Ok(()),
        Ok(true) => Err("git tracks files inside it, or could not list them".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Looks for `.git` (a directory, or a file for linked work trees) in
/// `dir` and its ancestors without running git.
fn in_git_work_tree(dir: &Path) -> bool {
    dir.ancestors()
        .any(|dir| std::fs::symlink_metadata(dir.join(".git")).is_ok())
}

/// More than `days` days between the Unix time `at` and now.
fn older_than(at: i64, days: u64) -> bool {
    now_secs() - at > days as i64 * SECS_PER_DAY
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

    /// Kinds 0 to 2 list idle projects (the CLI); 3 and 4 list every
    /// project's build output (the desktop).
    fn cancelling_provider(
        kind: usize,
        root: &Path,
        runner: Arc<CancellingRunner>,
    ) -> Box<dyn CleanProvider> {
        match kind {
            0 | 3 => {
                let mut p = NodeModules::with_runner(IDLE_DAYS, runner.clone());
                p.search_roots = vec![root.into()];
                p.all_projects = kind == 3;
                Box::new(p)
            }
            1 | 4 => {
                let mut p = RustTargets::with_runner(IDLE_DAYS, runner.clone());
                p.search_roots = vec![root.into()];
                p.all_projects = kind == 4;
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
        for kind in 0..5 {
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

    // ---------- build output (spec OC16, OC17) ----------

    /// `<root>/proj/<manifest>`, `days` old, beside an empty `<folder>/`.
    /// Returns (folder, manifest). Never exactly 7 days: the rules are strict.
    fn build_project(root: &Path, manifest: &str, folder: &str, days: u64) -> (PathBuf, PathBuf) {
        let proj = root.join("proj");
        fs::create_dir_all(proj.join(folder)).unwrap();
        let manifest = proj.join(manifest);
        fs::write(&manifest, b"{}").unwrap();
        let when = SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        fs::File::open(&manifest)
            .unwrap()
            .set_modified(when)
            .unwrap();
        (proj.join(folder), manifest)
    }

    fn rust_project(root: &Path, days: u64) -> (PathBuf, PathBuf) {
        build_project(root, "Cargo.toml", "target", days)
    }

    fn ls_files(runner: MockRunner, folder: &Path, status: i32, stdout: &str) -> MockRunner {
        let args = git_args(folder, &["ls-files", "--", "."]);
        runner.with_exit(GIT, &as_strs(&args), status, stdout)
    }

    /// The desktop's listing: every project.
    fn desktop(
        runner: &MockRunner,
        (folder, manifest): &(PathBuf, PathBuf),
    ) -> Result<Option<(RiskLevel, Vec<Evidence>)>> {
        assess_build_output(runner, folder, manifest, None, &ScanContext::unchecked())
    }

    /// The CLI's listing: projects idle for `IDLE_DAYS`.
    fn cli(
        runner: &MockRunner,
        (folder, manifest): &(PathBuf, PathBuf),
    ) -> Result<Option<(RiskLevel, Vec<Evidence>)>> {
        let ctx = ScanContext::unchecked();
        assess_build_output(runner, folder, manifest, Some(IDLE_DAYS), &ctx)
    }

    fn risk_of(found: Option<(RiskLevel, Vec<Evidence>)>) -> Option<RiskLevel> {
        found.map(|(risk, _)| risk)
    }

    #[test]
    fn desktop_marks_an_idle_project_outside_git_safe_without_running_git() {
        let project = rust_project(&tempdir("bo-idle"), 8);
        let runner = MockRunner::new();
        let (risk, evidence) = desktop(&runner, &project).unwrap().expect("offered");
        assert_eq!(risk, RiskLevel::Safe);
        assert!(matches!(
            evidence.as_slice(),
            [Evidence::ManifestModified { .. }, Evidence::NotGitRepo]
        ));
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn desktop_lists_an_active_project_as_review() {
        let project = rust_project(&tempdir("bo-active"), 2);
        let (risk, evidence) = desktop(&MockRunner::new(), &project)
            .unwrap()
            .expect("listed");
        assert_eq!(risk, RiskLevel::Review);
        assert!(matches!(
            evidence.as_slice(),
            [Evidence::ManifestModified { .. }]
        ));
        // The CLI still lists only projects idle for `--idle-days`.
        assert_eq!(cli(&MockRunner::new(), &project).unwrap(), None);
    }

    #[test]
    fn desktop_marks_an_idle_repo_project_safe_with_every_git_fact() {
        let root = tempdir("bo-repo-idle");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let runner = ls_files(
            mock_git(project.1.parent().unwrap(), 8, ""),
            &project.0,
            0,
            "",
        );
        let (risk, evidence) = desktop(&runner, &project).unwrap().expect("offered");
        assert_eq!(risk, RiskLevel::Safe);
        assert!(matches!(
            evidence.as_slice(),
            [
                Evidence::ManifestModified { .. },
                Evidence::LastCommit { .. },
                Evidence::WorkTreeClean
            ]
        ));
    }

    #[test]
    fn desktop_lists_a_recent_commit_or_uncommitted_changes_as_review() {
        let root = tempdir("bo-repo-active");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let dir = project.1.parent().unwrap();
        let recent = ls_files(mock_git(dir, 2, ""), &project.0, 0, "");
        assert_eq!(
            risk_of(desktop(&recent, &project).unwrap()),
            Some(RiskLevel::Review)
        );
        let dirty = ls_files(mock_git(dir, 40, " M src/lib.rs\n"), &project.0, 0, "");
        let (risk, evidence) = desktop(&dirty, &project).unwrap().expect("listed");
        assert_eq!(risk, RiskLevel::Review);
        assert!(!evidence.contains(&Evidence::WorkTreeClean));
    }

    #[test]
    fn desktop_lists_a_project_whose_activity_git_cannot_tell_as_review() {
        let root = tempdir("bo-unknown");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let dir = project.1.parent().unwrap();
        let log = git_args(dir, &["log", "-1", "--format=%ct", "--", "."]);
        let failed_log =
            ls_files(MockRunner::new(), &project.0, 0, "").with_exit(GIT, &as_strs(&log), 128, "");
        let (risk, evidence) = desktop(&failed_log, &project).unwrap().expect("listed");
        assert_eq!(risk, RiskLevel::Review);
        assert!(matches!(
            evidence.as_slice(),
            [Evidence::ManifestModified { .. }]
        ));
        let st = git_args(dir, &["status", "--porcelain", "--", "."]);
        let timeout = CommandError::Timeout {
            bin: GIT.to_string(),
            timeout_ms: 30_000,
        };
        let status_timeout = ls_files(mock_git(dir, 40, ""), &project.0, 0, "").with_output(
            GIT,
            &as_strs(&st),
            Err(timeout),
        );
        assert_eq!(
            risk_of(desktop(&status_timeout, &project).unwrap()),
            Some(RiskLevel::Review)
        );
    }

    #[test]
    fn build_output_holding_tracked_files_is_never_offered() {
        let root = tempdir("bo-tracked");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let dir = project.1.parent().unwrap();
        let runner = ls_files(mock_git(dir, 40, ""), &project.0, 0, "fixture.bin\n");
        assert_eq!(desktop(&runner, &project).unwrap(), None);
        assert_eq!(cli(&runner, &project).unwrap(), None);
    }

    #[test]
    fn build_output_is_not_offered_when_git_cannot_list_tracked_files() {
        let root = tempdir("bo-ls-fails");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let dir = project.1.parent().unwrap();
        let failed = ls_files(mock_git(dir, 40, ""), &project.0, 128, "");
        assert_eq!(desktop(&failed, &project).unwrap(), None);
        assert_eq!(cli(&failed, &project).unwrap(), None);
        let ls = git_args(&project.0, &["ls-files", "--", "."]);
        let timeout = CommandError::Timeout {
            bin: GIT.to_string(),
            timeout_ms: 30_000,
        };
        let timed_out = mock_git(dir, 40, "").with_output(GIT, &as_strs(&ls), Err(timeout));
        assert_eq!(desktop(&timed_out, &project).unwrap(), None);
    }

    #[test]
    fn a_build_output_folder_that_is_its_own_repo_is_checked() {
        let (folder, manifest) =
            build_project(&tempdir("bo-own-repo"), "package.json", "node_modules", 40);
        mark_repo(&folder);
        let runner = ls_files(MockRunner::new(), &folder, 0, "index.js\n");
        assert_eq!(desktop(&runner, &(folder, manifest)).unwrap(), None);
    }

    #[test]
    fn unavailable_git_fails_the_desktop_listing_of_a_repo_project() {
        let root = tempdir("bo-nogit");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let err = desktop(&MockRunner::new(), &project).unwrap_err();
        assert!(err.to_string().contains("git is unavailable"), "{err}");
    }

    #[test]
    fn the_cli_keeps_idle_build_output_review_with_the_same_evidence() {
        let root = tempdir("bo-cli");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let runner = ls_files(
            mock_git(project.1.parent().unwrap(), 40, ""),
            &project.0,
            0,
            "",
        );
        let (risk, evidence) = cli(&runner, &project).unwrap().expect("offered");
        assert_eq!(risk, RiskLevel::Review);
        assert_eq!(
            Some(evidence),
            assess(&runner, &project.1, IDLE_DAYS, &ScanContext::unchecked()).unwrap()
        );
    }

    #[test]
    fn the_guard_refuses_tracked_build_output_before_acting() {
        let root = tempdir("bo-guard");
        mark_repo(&root);
        let (folder, _) = rust_project(&root, 40);
        assert!(refuse_tracked(&ls_files(MockRunner::new(), &folder, 0, ""), &folder).is_ok());
        let tracked = ls_files(MockRunner::new(), &folder, 0, "fixture.bin\n");
        assert!(refuse_tracked(&tracked, &folder).is_err());
        let failed = ls_files(MockRunner::new(), &folder, 128, "");
        assert!(refuse_tracked(&failed, &folder).is_err());
        assert!(refuse_tracked(&MockRunner::new(), &folder)
            .unwrap_err()
            .contains("git is unavailable"));
        // A missing folder is left to the path checks, which report it.
        assert!(refuse_tracked(&MockRunner::new(), &root.join("gone")).is_ok());
    }
}
