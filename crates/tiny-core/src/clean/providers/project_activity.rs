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
use crate::runner::{CommandError, CommandOutcome, CommandRunner, TOOL_TIMEOUT};

/// Absolute on purpose: a Finder-launched app has a minimal `PATH`.
const GIT: &str = "/usr/bin/git";
/// Options for every git call on a repository we only inspect. It must not
/// write the index (`--no-optional-locks`), and no program its own config
/// names may run: a filesystem monitor, a hook or a signature check. `-c`
/// outranks the repository's config and reaches the processes git starts.
const INSPECT_ONLY: [&str; 7] = [
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "log.showSignature=false",
];
/// `status` without submodules: their worktrees and config are not ours to run.
pub(crate) const STATUS: [&str; 5] = [
    "status",
    "--porcelain",
    "--ignore-submodules=all",
    "--",
    ".",
];
/// Filter drivers `status` would run on a file it has to re-read.
const FILTER_DRIVERS: &str = r"^filter\..*\.(clean|process)$";
const SECS_PER_DAY: i64 = 86_400;

/// Days without edits, commits or uncommitted changes after which the
/// desktop's build output inside a git work tree is Safe, so the app
/// preselects it. Outside git it stays Review: a manifest's age alone does
/// not prove a project idle.
pub const BUILD_OUTPUT_SAFE_DAYS: u64 = 7;

/// Why build output is not offered when git cannot list its tracked files.
/// The app shows refusal reasons as they are.
const UNLISTED: &str = "git could not tell whether it holds tracked files";
/// Bound on the git error quoted in that reason.
const DETAIL_CHARS: usize = 200;

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
/// as Review. `None` (the desktop) offers every project: Safe when git shows
/// it idle for more than `BUILD_OUTPUT_SAFE_DAYS` with every fact checked,
/// Review when active, outside git, or when that cannot be told. Either way
/// a folder holding a file git tracks is never offered, nor one whose
/// tracked files git cannot list; that one is noted as refused, with why.
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
            // Outside git only the manifest's age is known (`NotGitRepo`).
            let risk = if idle && !evidence.contains(&Evidence::NotGitRepo) {
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
    if ctx.is_cancelled() {
        return Ok(None);
    }
    match tracked_files(runner, folder)? {
        Tracked::No => Ok(Some(offered)),
        Tracked::Yes => Ok(None),
        // Fail closed, but say why rather than drop the project silently.
        Tracked::Unknown(detail) => {
            ctx.note_refused(folder, unlisted(&detail));
            Ok(None)
        }
    }
}

/// What git says about the files it tracks inside a build output folder.
enum Tracked {
    No,
    Yes,
    /// Git did not answer; the detail says why.
    Unknown(String),
}

/// A folder outside every git work tree holds no tracked file and runs no
/// git. `Err` only when git cannot run at all.
fn tracked_files(runner: &dyn CommandRunner, folder: &Path) -> Result<Tracked> {
    // The folder's own `.git` counts: a `node_modules` may itself be a repo.
    if !in_git_work_tree(folder) {
        return Ok(Tracked::No);
    }
    let Some(folder_arg) = folder.to_str() else {
        return Ok(Tracked::Unknown(
            "its path is not UTF-8, so git was not asked".into(),
        ));
    };
    let listed = git_answer(runner, folder_arg, &["ls-files", "--", "."])?;
    Ok(match listed {
        Ok(files) if files.trim().is_empty() => Tracked::No,
        Ok(_) => Tracked::Yes,
        Err(detail) => Tracked::Unknown(detail),
    })
}

fn unlisted(detail: &str) -> String {
    format!("{UNLISTED} ({detail})")
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
    match tracked_files(runner, folder) {
        Ok(Tracked::No) => Ok(()),
        Ok(Tracked::Yes) => Err("git tracks files inside it".into()),
        Ok(Tracked::Unknown(detail)) => Err(unlisted(&detail)),
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

/// False when there are uncommitted changes or git cannot answer, and when
/// the repository's own config defines a filter `status` could run.
fn work_tree_clean(runner: &dyn CommandRunner, project: &str) -> Result<bool> {
    if !no_repository_filter(runner, project)? {
        return Ok(false);
    }
    let out = git(runner, project, &STATUS)?;
    Ok(out.is_some_and(|text| text.trim().is_empty()))
}

/// True when no `clean` or `process` filter comes from the repository's own
/// config (local or worktree, includes followed). Filters from the user's
/// global or system config, such as Git LFS, are the user's own. False when
/// git cannot tell.
fn no_repository_filter(runner: &dyn CommandRunner, project: &str) -> Result<bool> {
    let args = ["config", "--show-scope", "--get-regexp", FILTER_DRIVERS];
    match run_git(runner, project, &args) {
        Ok(outcome) if outcome.success() => Ok(outcome
            .stdout
            .lines()
            .all(|line| line.starts_with("global\t") || line.starts_with("system\t"))),
        // `--get-regexp` exits 1 when nothing matches.
        Ok(outcome) => Ok(outcome.status == Some(1)),
        Err(CommandError::Timeout { .. } | CommandError::NonUtf8(_)) => Ok(false),
        Err(CommandError::NotFound(_) | CommandError::Spawn { .. }) => Err(unavailable()),
    }
}

/// Stdout of a successful git call. A non-zero exit, timeout or non-UTF-8
/// output is `None`; only a git that cannot be started is an error.
fn git(runner: &dyn CommandRunner, project: &str, args: &[&str]) -> Result<Option<String>> {
    Ok(git_answer(runner, project, args)?.ok())
}

/// `git`, keeping why git did not answer.
fn git_answer(
    runner: &dyn CommandRunner,
    project: &str,
    args: &[&str],
) -> Result<std::result::Result<String, String>> {
    match run_git(runner, project, args) {
        Ok(outcome) if outcome.success() => Ok(Ok(outcome.stdout)),
        Ok(outcome) => Ok(Err(exit_detail(&outcome))),
        Err(e @ (CommandError::Timeout { .. } | CommandError::NonUtf8(_))) => {
            Ok(Err(e.to_string()))
        }
        Err(CommandError::NotFound(_) | CommandError::Spawn { .. }) => Err(unavailable()),
    }
}

/// `git <INSPECT_ONLY> -C <project> <args>`.
fn run_git(
    runner: &dyn CommandRunner,
    project: &str,
    args: &[&str],
) -> std::result::Result<CommandOutcome, CommandError> {
    let mut full = INSPECT_ONLY.to_vec();
    full.extend_from_slice(&["-C", project]);
    full.extend_from_slice(args);
    runner.output(GIT, &full, TOOL_TIMEOUT)
}

fn unavailable() -> crate::error::Error {
    engine_error!("git is unavailable, so projects inside git repositories cannot be checked")
}

/// The exit status and git's first stderr line, e.g. the `/usr/bin/git`
/// shim's "xcrun: error: invalid active developer path" without the
/// Command Line Tools.
fn exit_detail(outcome: &CommandOutcome) -> String {
    let status = match outcome.status {
        Some(code) => format!("git exited with status {code}"),
        None => "git was stopped by a signal".into(),
    };
    let first_line = outcome
        .stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty());
    match first_line {
        Some(line) => {
            let line: String = line.chars().take(DETAIL_CHARS).collect();
            format!("{status}: {line}")
        }
        None => status,
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

/// Git argument lists for mocks in this module and its providers.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use super::{FILTER_DRIVERS, GIT, INSPECT_ONLY};
    use crate::runner::test_support::MockRunner;

    /// `args` as `git_answer` sends them for `project`.
    pub(crate) fn git_args(project: &Path, args: &[&str]) -> Vec<String> {
        let project = project.to_str().unwrap();
        INSPECT_ONLY
            .iter()
            .chain(&["-C", project])
            .chain(args)
            .map(|s| s.to_string())
            .collect()
    }

    /// `runner`, answering that `project`'s config defines no filter.
    pub(crate) fn no_filters(runner: MockRunner, project: &Path) -> MockRunner {
        let args = git_args(
            project,
            &["config", "--show-scope", "--get-regexp", FILTER_DRIVERS],
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        runner.with_exit(GIT, &args, 1, "")
    }
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

    use super::test_support::{git_args, no_filters};

    /// Mock git answering `log` with `commit_secs_ago` and `status` with `dirty`.
    fn mock_git(project: &Path, commit_days_ago: i64, status: &str) -> MockRunner {
        let log = git_args(project, &["log", "-1", "--format=%ct", "--", "."]);
        let st = git_args(project, &STATUS);
        let ct = now_secs() - commit_days_ago * SECS_PER_DAY;
        no_filters(MockRunner::new(), project)
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

    /// A committed repository whose config runs `.git/probe.sh` as a
    /// filesystem monitor and, with `filter`, as a clean filter for every
    /// file. The script leaves `.git/ran` behind.
    fn hostile_repo(filter: bool) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        let setup = |args: &[&str]| {
            let status = std::process::Command::new(GIT)
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgSign=false",
                ])
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "git {args:?}");
        };
        fs::write(dir.join("file.txt"), "kept\n").unwrap();
        setup(&["init", "-q"]);
        setup(&["add", "file.txt"]);
        setup(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);
        let script = dir.join(".git/probe.sh");
        let ran = dir.join(".git/ran");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", ran.display()),
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();
        let script = script.to_str().unwrap();
        setup(&["config", "core.fsmonitor", script]);
        if filter {
            fs::write(dir.join(".git/info/attributes"), "* filter=probe\n").unwrap();
            setup(&["config", "filter.probe.clean", script]);
        }
        // An mtime equal to the index's makes git re-read the file through filters.
        let index = fs::metadata(dir.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap();
        fs::File::open(dir.join("file.txt"))
            .unwrap()
            .set_modified(index)
            .unwrap();
        repo
    }

    #[test]
    fn inspecting_a_repository_runs_no_program_its_config_names() {
        if !Path::new(GIT).exists() {
            return;
        }
        let repo = hostile_repo(false);
        let dir = repo.path().to_str().unwrap();
        let runner = crate::runner::RealRunner;
        assert!(last_commit(&runner, dir).unwrap().is_some());
        assert!(work_tree_clean(&runner, dir).unwrap());
        assert!(!repo.path().join(".git/ran").exists(), "the fsmonitor ran");
    }

    #[test]
    fn a_repository_defining_its_own_filter_is_not_asked_for_status() {
        if !Path::new(GIT).exists() {
            return;
        }
        let repo = hostile_repo(true);
        let dir = repo.path().to_str().unwrap();
        assert!(!work_tree_clean(&crate::runner::RealRunner, dir).unwrap());
        assert!(
            !repo.path().join(".git/ran").exists(),
            "the clean filter ran"
        );
    }

    #[test]
    fn filters_from_the_users_own_config_still_allow_status() {
        let project = Path::new("/p");
        let args = git_args(
            project,
            &["config", "--show-scope", "--get-regexp", FILTER_DRIVERS],
        );
        let answer = |stdout: &str| {
            let runner = MockRunner::new()
                .with_exit(GIT, &as_strs(&args), 0, stdout)
                .with_exit(GIT, &as_strs(&git_args(project, &STATUS)), 0, "");
            work_tree_clean(&runner, "/p").unwrap()
        };
        assert!(answer("global\tfilter.lfs.clean git-lfs clean -- %f\nsystem\tfilter.lfs.process git-lfs filter-process\n"));
        assert!(!answer(
            "global\tfilter.lfs.clean git-lfs clean -- %f\nlocal\tfilter.x.clean ./x\n"
        ));
        assert!(!answer("worktree\tfilter.x.process ./x\n"));
    }

    #[test]
    fn git_status_failure_is_not_offered() {
        let root = tempdir("status-fails");
        mark_repo(&root);
        let manifest = old_manifest(&root);
        let project = manifest.parent().unwrap();
        let st = git_args(project, &STATUS);
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
    fn desktop_lists_an_idle_project_outside_git_as_review_without_running_git() {
        // Only the manifest's age is known outside git; it does not prove idle.
        let project = rust_project(&tempdir("bo-idle"), 40);
        let runner = MockRunner::new();
        let (risk, evidence) = desktop(&runner, &project).unwrap().expect("listed");
        assert_eq!(risk, RiskLevel::Review);
        assert!(matches!(
            evidence.as_slice(),
            [Evidence::ManifestModified { .. }, Evidence::NotGitRepo]
        ));
        assert!(runner.output_calls.lock().unwrap().is_empty());
        // The CLI still offers it, as before.
        let (risk, evidence) = cli(&runner, &project).unwrap().expect("offered");
        assert_eq!(risk, RiskLevel::Review);
        assert_eq!(evidence.last(), Some(&Evidence::NotGitRepo));
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
        let st = git_args(dir, &STATUS);
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
        let ctx = ScanContext::unchecked();
        assert_eq!(
            assess_build_output(&runner, &project.0, &project.1, None, &ctx).unwrap(),
            None
        );
        assert_eq!(cli(&runner, &project).unwrap(), None);
        // The user's own files: left out without a refusal note.
        assert!(ctx.take_findings().refused.is_empty());
    }

    /// `ls-files` in `folder` exits 1 the way the `/usr/bin/git` shim does
    /// without the Command Line Tools.
    fn ls_files_without_clt(runner: MockRunner, folder: &Path) -> MockRunner {
        let args = git_args(folder, &["ls-files", "--", "."]);
        let outcome = CommandOutcome {
            status: Some(1),
            stdout: String::new(),
            stderr: format!("{XCRUN_ERROR}\nsecond line\n"),
        };
        runner.with_output(GIT, &as_strs(&args), Ok(outcome))
    }

    const XCRUN_ERROR: &str =
        "xcrun: error: invalid active developer path (/Library/Developer/CommandLineTools)";

    #[test]
    fn build_output_git_cannot_list_is_refused_with_the_reason() {
        let root = tempdir("bo-ls-fails");
        mark_repo(&root);
        let project = rust_project(&root, 40);
        let dir = project.1.parent().unwrap();
        let refusals = |runner: &MockRunner, idle_days| {
            let ctx = ScanContext::unchecked();
            let listed =
                assess_build_output(runner, &project.0, &project.1, idle_days, &ctx).unwrap();
            assert_eq!(listed, None, "never offered");
            ctx.take_findings().refused
        };
        let shim = ls_files_without_clt(mock_git(dir, 40, ""), &project.0);
        for idle_days in [None, Some(IDLE_DAYS)] {
            let refused = refusals(&shim, idle_days);
            let reason = &refused[&project.0];
            assert!(reason.starts_with(UNLISTED), "{reason}");
            assert!(reason.contains(XCRUN_ERROR), "{reason}");
            assert!(!reason.contains("second line"), "{reason}");
        }
        let failed = ls_files(mock_git(dir, 40, ""), &project.0, 128, "");
        assert!(refusals(&failed, None)[&project.0].contains("status 128"));
        let ls = git_args(&project.0, &["ls-files", "--", "."]);
        let timeout = CommandError::Timeout {
            bin: GIT.to_string(),
            timeout_ms: 30_000,
        };
        let timed_out = mock_git(dir, 40, "").with_output(GIT, &as_strs(&ls), Err(timeout));
        assert!(refusals(&timed_out, None)[&project.0].contains("did not finish"));
    }

    #[test]
    fn a_category_says_which_projects_git_could_not_check() {
        let root = tempdir("bo-refused-category");
        mark_repo(&root);
        let (folder, manifest) = rust_project(&root, 40);
        let runner = ls_files_without_clt(mock_git(manifest.parent().unwrap(), 40, ""), &folder);
        let opts = crate::options::CleanOptions {
            all_build_output: true,
            ..Default::default()
        };
        let mut provider = RustTargets::with_options(&opts, Arc::new(runner));
        provider.search_roots = vec![root.clone()];
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(provider)];
        let probe = MockChecker::none();
        let report = discover_checked(&providers, &ScanContext::new(None, &probe), None);
        match &report.categories[0].outcome {
            crate::clean::discover::CategoryOutcome::Found { items, refused, .. } => {
                assert!(items.is_empty());
                assert_eq!(refused.len(), 1);
                assert_eq!(refused[0].0, folder);
                assert!(refused[0].1.contains(XCRUN_ERROR), "{}", refused[0].1);
            }
            other => panic!("unexpected {other:?}"),
        }
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
        let failed = ls_files_without_clt(MockRunner::new(), &folder);
        assert!(refuse_tracked(&failed, &folder)
            .unwrap_err()
            .starts_with(UNLISTED));
        assert!(refuse_tracked(&MockRunner::new(), &folder)
            .unwrap_err()
            .contains("git is unavailable"));
        // A missing folder is left to the path checks, which report it.
        assert!(refuse_tracked(&MockRunner::new(), &root.join("gone")).is_ok());
    }
}
