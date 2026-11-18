//! Real git on fixtures in temporary folders (AC-W2, AC-W3): which worktrees
//! are listed, and each row of the status table. Nothing outside the
//! fixtures is read or written, apart from `xcode-select -p` and the list of
//! processes' folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;
use crate::processes::working_dir::all_working_directories;
use crate::runner::test_support::MockRunner;
use crate::runner::RealRunner;

const DAY: i64 = SECS_PER_DAY;
const CLAUDE: &str = "claudeCode";

/// Fixture git, away from the user's own config and hooks.
pub(super) fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, args, None)
}

/// `git`, dating a commit at Unix time `at` when given.
fn git_at(dir: &Path, args: &[&str], at: Option<i64>) -> String {
    let date = at.map(|at| format!("@{at} +0000"));
    let out = std::process::Command::new("/usr/bin/git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "protocol.file.allow=always"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(
            date.iter()
                .flat_map(|d| [("GIT_AUTHOR_DATE", d), ("GIT_COMMITTER_DATE", d)]),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

pub(super) fn commit(dir: &Path, file: &str) {
    fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-qm", file]);
}

pub(super) fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub(super) struct Fixture {
    _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) repo: PathBuf,
    memory: RepoMemory,
}

impl Fixture {
    /// A repository with one commit on `base`, remembered as seen now.
    pub(super) fn new(base: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", base]);
        commit(&repo, "first");
        let memory = RepoMemory::at(root.join("state/agent-repos.json"));
        let fixture = Self {
            _dir: dir,
            root,
            repo,
            memory,
        };
        fixture.seen(&fixture.repo.clone(), now());
        fixture
    }

    fn seen(&self, folder: &Path, at: i64) {
        let sighting = Sighting {
            folder: Some(folder.to_path_buf()),
            tool: CLAUDE.into(),
        };
        self.memory.remember(&[sighting], at).unwrap();
    }

    /// `git worktree add` of a new branch at `name` beside the repository.
    pub(super) fn branch(&self, name: &str, branch: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
        path
    }

    fn detached(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.repo,
            &["worktree", "add", "-q", "--detach", path.to_str().unwrap()],
        );
        path
    }

    fn list_with(&self, cwds: &[PathBuf], sessions: &[Sighting], home: &Path) -> Listing {
        list(&ListInputs {
            runner: &RealRunner,
            memory: &self.memory,
            home,
            working_directories: cwds,
            sessions,
            now: now(),
        })
    }

    pub(super) fn list(&self) -> Listing {
        self.list_with(&[], &[], &self.root)
    }

    /// `(folder name, status)` of every listed worktree.
    fn statuses(&self) -> Vec<(String, WorktreeStatus)> {
        rows(&self.list())
    }
}

fn rows(listing: &Listing) -> Vec<(String, WorktreeStatus)> {
    let Listing::Repos(repos) = listing else {
        panic!("git is missing: {listing:?}");
    };
    repos
        .iter()
        .flat_map(|repo| &repo.worktrees)
        .map(|w| {
            let name = w.path.file_name().unwrap().to_string_lossy().into_owned();
            (name, w.status.clone())
        })
        .collect()
}

fn status_of(rows: &[(String, WorktreeStatus)], name: &str) -> WorktreeStatus {
    rows.iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("{name} not listed in {rows:?}"))
        .1
        .clone()
}

/// Sets the worktree's `HEAD` and index file times `days` back.
fn age(worktree: &Path, days: u64) {
    let admin = repo::pointed_git_dir(worktree).unwrap();
    let then = SystemTime::now() - Duration::from_secs(days * DAY as u64);
    for file in ["HEAD", "index"] {
        let file = fs::File::options()
            .write(true)
            .open(admin.join(file))
            .unwrap();
        file.set_modified(then).unwrap();
    }
}

#[test]
fn the_main_checkout_and_prunable_entries_are_never_listed() {
    let f = Fixture::new("main");
    f.branch("wt-keep", "keep");
    let gone = f.branch("wt-gone", "gone");
    fs::remove_dir_all(&gone).unwrap();
    let rows = f.statuses();
    let names: Vec<_> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["wt-keep"]);
}

#[test]
fn a_repository_is_remembered_for_thirty_days_across_a_relaunch() {
    let f = Fixture::new("main");
    f.branch("wt", "feat");
    let other = Fixture::new("main");
    other.branch("wt-old", "old");
    // One list, two repositories: one seen 29 days ago, one 31.
    let memory = RepoMemory::at(f.root.join("relaunch.json"));
    memory
        .remember(&[sighting(&f.repo)], now() - 29 * DAY)
        .unwrap();
    memory
        .remember(&[sighting(&other.repo)], now() - 31 * DAY)
        .unwrap();
    let relaunched = RepoMemory::at(memory.file().to_path_buf());
    let listing = list(&ListInputs {
        runner: &RealRunner,
        memory: &relaunched,
        home: &f.root,
        working_directories: &[],
        sessions: &[],
        now: now(),
    });
    let names: Vec<_> = rows(&listing).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["wt"]);
}

fn sighting(folder: &Path) -> Sighting {
    Sighting {
        folder: Some(folder.to_path_buf()),
        tool: CLAUDE.into(),
    }
}

#[test]
fn paths_with_spaces_and_unicode_are_listed_and_matched_to_a_session() {
    let f = Fixture::new("main");
    // Composed é in the folder's name, decomposed in the session's path.
    let wt = f.branch("wt \u{e9} space", "accent");
    let decomposed = f.root.join("wt e\u{301} space").join("src");
    fs::create_dir_all(wt.join("src")).unwrap();
    let session = Sighting {
        folder: Some(decomposed.clone()),
        tool: "codex".into(),
    };
    let listing = f.list_with(&[decomposed], &[session], &f.root);
    let Listing::Repos(repos) = &listing else {
        panic!("git is missing")
    };
    let row = &repos[0].worktrees[0];
    assert_eq!(row.path.file_name().unwrap(), "wt \u{e9} space");
    assert_eq!(row.status, WorktreeStatus::ActiveSession);
    assert_eq!(row.tool.as_deref(), Some("codex"));
}

#[test]
fn a_copied_git_directory_lists_none_of_the_original_worktrees() {
    let f = Fixture::new("main");
    f.branch("wt", "feat");
    let mirror = f.root.join("mirror");
    fs::create_dir(&mirror).unwrap();
    let copied = std::process::Command::new("/bin/cp")
        .args(["-R"])
        .arg(f.repo.join(".git"))
        .arg(mirror.join(".git"))
        .status()
        .unwrap();
    assert!(copied.success());
    f.seen(&mirror, now());
    let Listing::Repos(repos) = f.list() else {
        panic!("git is missing")
    };
    let listed: Vec<_> = repos
        .iter()
        .map(|r| (r.name.clone(), r.worktrees.len()))
        .collect();
    // The mirror lists nothing, so it has no group; the worktree appears once.
    assert_eq!(listed, [("repo".to_string(), 1)]);
}

#[test]
fn every_row_of_the_status_table() {
    let f = Fixture::new("main");
    let merged = f.branch("merged", "merged");
    commit(&merged, "merged-work");
    git(&f.repo, &["merge", "-q", "--ff-only", "merged"]);
    f.branch("no-own-commits", "no-own");
    let unpushed = f.branch("unpushed", "unpushed");
    commit(&unpushed, "local-only");
    let detached = f.detached("detached");
    commit(&detached, "lost-if-moved");
    let dirty = f.branch("dirty", "dirty");
    fs::write(dirty.join("untracked.txt"), "new").unwrap();
    let locked = f.branch("locked", "locked");
    git(
        &f.repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "tool x",
            locked.to_str().unwrap(),
        ],
    );
    let claude = f.repo.join(".claude/worktrees/agent-1");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            claude.to_str().unwrap(),
        ],
    );
    // Ten days since its last commit and index change; the commit is also on
    // another branch, so nothing is lost and it is not Unpushed.
    let old = f.branch("old", "old");
    fs::write(old.join("old.txt"), "old").unwrap();
    git(&old, &["add", "--", "old.txt"]);
    git_at(&old, &["commit", "-qm", "old"], Some(now() - 10 * DAY - 60));
    git(&f.repo, &["branch", "old-copy", "old"]);
    age(&old, 10);
    let busy = f.branch("busy", "busy");
    fs::create_dir(busy.join("deep")).unwrap();

    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .current_dir(busy.join("deep"))
        .spawn()
        .unwrap();
    let cwds = all_working_directories();
    child.kill().unwrap();
    child.wait().unwrap();
    let listing = f.list_with(&cwds, &[], &f.root);
    let rows = rows(&listing);

    assert_eq!(status_of(&rows, "merged"), WorktreeStatus::Merged);
    assert!(matches!(
        status_of(&rows, "no-own-commits"),
        WorktreeStatus::LastChange { .. }
    ));
    assert_eq!(status_of(&rows, "unpushed"), WorktreeStatus::Unpushed);
    assert_eq!(
        status_of(&rows, "detached"),
        WorktreeStatus::DetachedCommits
    );
    assert_eq!(status_of(&rows, "dirty"), WorktreeStatus::Uncommitted);
    assert_eq!(
        status_of(&rows, "locked"),
        WorktreeStatus::LockedByGit {
            reason: Some("tool x".into())
        }
    );
    assert_eq!(
        status_of(&rows, "agent-1"),
        WorktreeStatus::Managed(ManagedBy::ClaudeCode)
    );
    assert_eq!(
        status_of(&rows, "old"),
        WorktreeStatus::LastChange { days: 10 }
    );
    assert_eq!(status_of(&rows, "busy"), WorktreeStatus::ActiveSession);

    // Merged rows start ticked, nothing else does.
    let ticked: Vec<_> = rows
        .iter()
        .filter(|(_, s)| s.ticked_by_default())
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(ticked, ["merged"]);
}

#[test]
fn a_dirty_submodule_makes_the_worktree_uncommitted() {
    let f = Fixture::new("main");
    let wt = f.branch("with-sub", "with-sub");
    let sub = wt.join("vendor");
    fs::create_dir(&sub).unwrap();
    git(&sub, &["init", "-q", "-b", "main"]);
    commit(&sub, "lib");
    fs::write(
        wt.join(".gitmodules"),
        "[submodule \"vendor\"]\n\tpath = vendor\n\turl = ./vendor\n",
    )
    .unwrap();
    git(&wt, &["add", "--", ".gitmodules", "vendor"]);
    git(&wt, &["commit", "-qm", "sub"]);
    git(&f.repo, &["merge", "-q", "--ff-only", "with-sub"]);
    assert_eq!(status_of(&f.statuses(), "with-sub"), WorktreeStatus::Merged);

    fs::write(sub.join("lib"), "changed").unwrap();
    assert_eq!(
        status_of(&f.statuses(), "with-sub"),
        WorktreeStatus::Uncommitted
    );
    git(&sub, &["commit", "-qam", "moved"]);
    assert_eq!(
        status_of(&f.statuses(), "with-sub"),
        WorktreeStatus::Uncommitted
    );
}

#[test]
fn master_alone_is_the_base_branch() {
    let f = Fixture::new("master");
    let wt = f.branch("done", "done");
    commit(&wt, "work");
    git(&f.repo, &["merge", "-q", "--ff-only", "done"]);
    let git_dir = f.repo.join(".git");
    assert_eq!(
        status::base_branch(&RealRunner, &git_dir).as_deref(),
        Some("refs/heads/master")
    );
    assert_eq!(status_of(&f.statuses(), "done"), WorktreeStatus::Merged);
}

#[test]
fn origin_head_names_the_base_and_its_local_branch_wins() {
    let f = Fixture::new("main");
    let head = git(&f.repo, &["rev-parse", "HEAD"]);
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/trunk", head.trim()],
    );
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let git_dir = f.repo.join(".git");
    assert_eq!(
        status::base_branch(&RealRunner, &git_dir).as_deref(),
        Some("refs/remotes/origin/trunk")
    );
    git(&f.repo, &["branch", "trunk"]);
    assert_eq!(
        status::base_branch(&RealRunner, &git_dir).as_deref(),
        Some("refs/heads/trunk")
    );
}

#[test]
fn a_session_whose_folder_cannot_be_read_locks_every_row() {
    let f = Fixture::new("main");
    let wt = f.branch("merged", "merged");
    commit(&wt, "work");
    git(&f.repo, &["merge", "-q", "--ff-only", "merged"]);
    let unreadable = Sighting {
        folder: None,
        tool: CLAUDE.into(),
    };
    let rows = rows(&f.list_with(&[], &[unreadable], &f.root));
    assert_eq!(status_of(&rows, "merged"), WorktreeStatus::ActiveSession);
}

#[test]
fn another_volume_and_a_codex_folder_are_locked() {
    let f = Fixture::new("main");
    f.branch("elsewhere", "elsewhere");
    // `/dev` is its own file system, so the fixture is not on "home's" volume.
    let away = rows(&f.list_with(&[], &[], Path::new("/dev")));
    assert_eq!(status_of(&away, "elsewhere"), WorktreeStatus::OtherVolume);

    let home = f.root.join("home");
    let codex = home.join(".codex/worktrees/a1b2");
    fs::create_dir_all(&codex).unwrap();
    let wt = codex.join("repo");
    git(
        &f.repo,
        &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
    );
    let managed = rows(&f.list_with(&[], &[], &home));
    assert_eq!(
        status_of(&managed, "repo"),
        WorktreeStatus::Managed(ManagedBy::Codex)
    );
}

#[test]
fn a_repository_filter_is_never_run_and_the_row_stays_locked() {
    let f = Fixture::new("main");
    f.branch("filtered", "filtered");
    git(&f.repo, &["config", "filter.probe.clean", "cat"]);
    assert!(matches!(
        status_of(&f.statuses(), "filtered"),
        WorktreeStatus::NotChecked { .. }
    ));
}

#[test]
fn without_the_command_line_tools_git_is_never_started() {
    let f = Fixture::new("main");
    f.branch("wt", "feat");
    let runner = MockRunner::new().with_exit(XCODE_SELECT, &["-p"], 2, "");
    let listing = list(&ListInputs {
        runner: &runner,
        memory: &f.memory,
        home: &f.root,
        working_directories: &[],
        sessions: &[],
        now: now(),
    });
    assert!(matches!(listing, Listing::NoGit { .. }));
    let calls = runner.output_calls.lock().unwrap();
    assert!(
        calls.iter().all(|c| !c.starts_with("/usr/bin/git")),
        "{calls:?}"
    );
}

#[test]
fn no_worktrees_is_an_empty_list() {
    let f = Fixture::new("main");
    assert_eq!(f.list(), Listing::Repos(Vec::new()));
}
