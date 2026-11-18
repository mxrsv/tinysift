//! Real git on fixtures in temporary folders (AC-W4): a worktree moved by
//! [`trash::move_to_trash`] leaves its place, its branch can be deleted and
//! git no longer lists it; one that became locked, changed or is kept stays.
//!
//! The Trash is a folder inside the fixture ([`FolderTrash`]), never the
//! user's: a real Trash move would leave fixture folders in the owner's
//! Trash, and the move's checks and git's view after it are the same for any
//! route that takes the folder away from its place.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use super::tests::{commit, git, now, Fixture};
use super::trash::{
    move_to_trash, Machine, MoveInputs, MoveOutcome, MoveReport, PlannedWorktree, Refusal,
    PROGRESS_MOVED,
};
use super::{Listing, WorktreeStatus};
use crate::clean::checked_execute::StopReason;
use crate::clean::finder_trash::{Trash, TrashError};
use crate::clean::protection::RuleScope;
use crate::clean::protection_store::ProtectionStore;
use crate::progress::Progress;
use crate::runner::RealRunner;

/// A Trash that is a folder of the fixture: a move renames into it.
struct FolderTrash {
    dir: PathBuf,
    /// Paths it refuses, as the Trash would a folder it cannot take.
    refuses: Vec<PathBuf>,
    moved: Mutex<Vec<PathBuf>>,
}

impl FolderTrash {
    fn new(root: &Path) -> Self {
        let dir = root.join("Trash");
        fs::create_dir(&dir).unwrap();
        Self {
            dir,
            refuses: Vec::new(),
            moved: Mutex::new(Vec::new()),
        }
    }

    fn moved(&self) -> Vec<PathBuf> {
        self.moved.lock().unwrap().clone()
    }
}

impl Trash for FolderTrash {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        if self.refuses.iter().any(|p| p == path) {
            return Err(TrashError::Os {
                code: 13,
                detail: "Permission denied".into(),
            });
        }
        let to = self.dir.join(path.file_name().unwrap());
        fs::rename(path, to).map_err(|e| TrashError::Failed(e.to_string()))?;
        self.moved.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

/// The processes' folders the move reads again, as the test sets them.
#[derive(Default)]
struct Processes {
    folders: Vec<PathBuf>,
    unreadable: bool,
}

impl Machine for Processes {
    fn working_directories(&self) -> Vec<PathBuf> {
        self.folders.clone()
    }

    fn unreadable_session(&self) -> bool {
        self.unreadable
    }
}

struct Run {
    fixture: Fixture,
    store: ProtectionStore,
    trash: FolderTrash,
    processes: Processes,
}

impl Run {
    fn new() -> Self {
        let fixture = Fixture::new("main");
        let store = ProtectionStore::at(fixture.root.join("rules"));
        let trash = FolderTrash::new(&fixture.root);
        Self {
            fixture,
            store,
            trash,
            processes: Processes::default(),
        }
    }

    /// Every listed worktree, as the app's listing hands it to a move.
    fn plan(&self) -> Vec<PlannedWorktree> {
        let Listing::Repos(repos) = self.fixture.list() else {
            panic!("git is missing");
        };
        repos
            .iter()
            .flat_map(|repo| {
                repo.worktrees.iter().map(|w| {
                    PlannedWorktree::of(&repo.git_dir, &w.path, w.branch.clone(), w.head.clone())
                        .unwrap()
                })
            })
            .collect()
    }

    fn run(&self, plan: &[PlannedWorktree], cancel: Option<&AtomicBool>) -> MoveReport {
        let lease = self.store.lease().unwrap();
        move_to_trash(
            plan,
            &MoveInputs {
                runner: &RealRunner,
                home: &self.fixture.root,
                trash: &self.trash,
                machine: &self.processes,
                policy: &lease,
                cancel,
                progress: None,
                now: now(),
            },
        )
    }

    fn path(&self, name: &str) -> PathBuf {
        self.fixture.root.join(name)
    }

    /// `git branch -d`, whether it succeeded.
    fn deletes_branch(&self, branch: &str) -> bool {
        std::process::Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&self.fixture.repo)
            .args(["branch", "-d", branch])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
            .status
            .success()
    }

    fn git_lists(&self, path: &Path) -> bool {
        git(&self.fixture.repo, &["worktree", "list", "--porcelain"])
            .lines()
            .any(|line| line == format!("worktree {}", path.display()))
    }
}

fn outcome_of<'a>(report: &'a MoveReport, path: &Path) -> &'a MoveOutcome {
    &report
        .results
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("{} not in {report:?}", path.display()))
        .outcome
}

#[test]
fn a_moved_worktree_leaves_its_place_frees_its_branch_and_leaves_git() {
    let run = Run::new();
    // `merged`: a commit of its own, on main since. `idle`: no commit of its own.
    let merged = run.fixture.branch("merged", "agent/merged");
    commit(&merged, "work");
    git(
        &run.fixture.repo,
        &["merge", "-q", "--ff-only", "agent/merged"],
    );
    let idle = run.fixture.branch("idle", "agent/idle");
    let plan = run.plan();
    assert_eq!(plan.len(), 2);

    let report = run.run(&plan, None);

    assert_eq!(report.stopped, None);
    assert!(report.prune_failures.is_empty(), "{report:?}");
    for path in [&merged, &idle] {
        assert_eq!(outcome_of(&report, path), &MoveOutcome::Moved);
        assert!(
            fs::symlink_metadata(path).is_err(),
            "{} left its place",
            path.display()
        );
        assert!(
            run.trash.dir.join(path.file_name().unwrap()).is_dir(),
            "and is in the Trash"
        );
        assert!(
            !run.git_lists(path),
            "git no longer lists {}",
            path.display()
        );
    }
    assert!(
        run.deletes_branch("agent/merged"),
        "the merged branch deletes"
    );
    assert!(run.deletes_branch("agent/idle"), "the idle branch deletes");
}

#[test]
fn a_row_that_became_locked_since_the_listing_is_skipped_with_its_status() {
    let mut run = Run::new();
    let clean = run.fixture.branch("clean", "agent/clean");
    let dirty = run.fixture.branch("dirty", "agent/dirty");
    let held = run.fixture.branch("held", "agent/held");
    let busy = run.fixture.branch("busy", "agent/busy");
    let plan = run.plan();

    // After the listing: a file appears, git locks one, a process starts in one.
    fs::write(dirty.join("notes.txt"), "draft").unwrap();
    git(
        &run.fixture.repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "tool x",
            held.to_str().unwrap(),
        ],
    );
    run.processes.folders = vec![busy.clone()];
    let report = run.run(&plan, None);

    assert_eq!(outcome_of(&report, &clean), &MoveOutcome::Moved);
    assert_eq!(
        outcome_of(&report, &dirty),
        &MoveOutcome::Refused(Refusal::Locked(WorktreeStatus::Uncommitted))
    );
    assert_eq!(
        outcome_of(&report, &held),
        &MoveOutcome::Refused(Refusal::Locked(WorktreeStatus::LockedByGit {
            reason: Some("tool x".into())
        }))
    );
    assert_eq!(
        outcome_of(&report, &busy),
        &MoveOutcome::Refused(Refusal::Locked(WorktreeStatus::ActiveSession))
    );
    assert_eq!(run.trash.moved(), std::slice::from_ref(&clean));
    for stayed in [&dirty, &held, &busy] {
        assert!(
            stayed.is_dir() && run.git_lists(stayed),
            "{} stays",
            stayed.display()
        );
    }
    assert!(!run.git_lists(&clean));
}

#[test]
fn a_session_whose_folder_cannot_be_read_locks_every_move() {
    let mut run = Run::new();
    let wt = run.fixture.branch("wt", "agent/wt");
    let plan = run.plan();
    run.processes.unreadable = true;
    let report = run.run(&plan, None);
    assert_eq!(
        outcome_of(&report, &wt),
        &MoveOutcome::Refused(Refusal::Locked(WorktreeStatus::ActiveSession))
    );
    assert!(run.trash.moved().is_empty() && wt.is_dir());
}

#[test]
fn a_worktree_that_is_not_the_one_listed_stays() {
    let run = Run::new();
    let moved_on = run.fixture.branch("moved-on", "agent/moved-on");
    let replaced = run.fixture.branch("replaced", "agent/replaced");
    let gone = run.fixture.branch("gone", "agent/gone");
    let plan = run.plan();

    // Another commit; the same path made again as another folder; the folder removed.
    commit(&moved_on, "more");
    let aside = run.path("aside");
    fs::rename(&replaced, &aside).unwrap();
    fs::create_dir(&replaced).unwrap();
    fs::copy(aside.join(".git"), replaced.join(".git")).unwrap();
    fs::remove_dir_all(&gone).unwrap();
    let report = run.run(&plan, None);

    assert_eq!(
        outcome_of(&report, &moved_on),
        &MoveOutcome::Refused(Refusal::Changed)
    );
    assert_eq!(
        outcome_of(&report, &replaced),
        &MoveOutcome::Refused(Refusal::Changed)
    );
    assert_eq!(
        outcome_of(&report, &gone),
        &MoveOutcome::Refused(Refusal::Gone)
    );
    assert!(run.trash.moved().is_empty());
    assert!(run.git_lists(&moved_on) && run.git_lists(&replaced));
}

#[test]
fn a_kept_folder_and_a_symlinked_path_stay() {
    let run = Run::new();
    let kept = run.fixture.branch("kept", "agent/kept");
    let other = run.fixture.branch("other", "agent/other");
    let mut plan = run.plan();
    {
        let mut lease = run.store.lease().unwrap();
        let revision = lease.snapshot().revision;
        lease.add(revision, &kept, RuleScope::Directory).unwrap();
    }
    // The same worktree reached through a symlinked parent is not its own root.
    let link = run.path("link");
    std::os::unix::fs::symlink(&run.fixture.root, &link).unwrap();
    let through = plan
        .iter()
        .find(|p| p.path == other)
        .map(|p| PlannedWorktree {
            path: link.join("other"),
            ..p.clone()
        })
        .unwrap();
    plan.retain(|p| p.path != other);
    plan.push(through);
    let report = run.run(&plan, None);

    assert_eq!(
        outcome_of(&report, &kept),
        &MoveOutcome::Refused(Refusal::Kept)
    );
    assert!(matches!(
        outcome_of(&report, &link.join("other")),
        MoveOutcome::Refused(_)
    ));
    assert!(run.trash.moved().is_empty() && kept.is_dir() && other.is_dir());
}

#[test]
fn a_refused_move_stays_with_its_worktree_and_the_rest_still_go() {
    let mut run = Run::new();
    let stuck = run.fixture.branch("stuck", "agent/stuck");
    let fine = run.fixture.branch("fine", "agent/fine");
    let plan = run.plan();
    run.trash.refuses = vec![stuck.clone()];
    let report = run.run(&plan, None);

    assert!(matches!(
        outcome_of(&report, &stuck),
        MoveOutcome::Failed {
            source_present: true,
            ..
        }
    ));
    assert_eq!(outcome_of(&report, &fine), &MoveOutcome::Moved);
    assert!(run.git_lists(&stuck) && !run.git_lists(&fine));
    assert!(run.deletes_branch("agent/fine") && !run.deletes_branch("agent/stuck"));
}

#[test]
fn a_cancelled_run_moves_nothing_and_each_move_is_reported_as_it_happens() {
    let run = Run::new();
    let a = run.fixture.branch("a", "agent/a");
    let plan = run.plan();
    let cancel = AtomicBool::new(true);
    let report = run.run(&plan, Some(&cancel));
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(outcome_of(&report, &a), &MoveOutcome::NotAttempted);
    assert!(a.is_dir() && run.trash.moved().is_empty());

    let seen: Mutex<Vec<Progress>> = Mutex::new(Vec::new());
    let record = |p: Progress| seen.lock().unwrap().push(p);
    let lease = run.store.lease().unwrap();
    let report = move_to_trash(
        &plan,
        &MoveInputs {
            runner: &RealRunner,
            home: &run.fixture.root,
            trash: &run.trash,
            machine: &run.processes,
            policy: &lease,
            cancel: None,
            progress: Some(&record),
            now: now(),
        },
    );
    assert_eq!(outcome_of(&report, &a), &MoveOutcome::Moved);
    let moved: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.operation == PROGRESS_MOVED)
        .map(|p| p.message.clone())
        .collect();
    assert_eq!(moved, [a.to_string_lossy().into_owned()]);
}
