//! The consent contract of `worktree_move` on git fixtures in a temporary
//! folder: only listed worktrees move, once, behind the gate. The Trash is a
//! folder of the fixture, never the user's.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::worktrees::FfiWorktreeList;
use crate::FfiProgress;

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("/usr/bin/git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status
        .success()
}

struct FolderTrash(PathBuf);

impl Trash for FolderTrash {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        fs::rename(path, self.0.join(path.file_name().unwrap()))
            .map_err(|e| TrashError::Failed(e.to_string()))
    }
}

#[derive(Default)]
struct NoProcesses;

impl Machine for NoProcesses {
    fn working_directories(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn unreadable_session(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct Listener(Mutex<Vec<FfiProgress>>);

impl ProgressListener for Listener {
    fn on_progress(&self, progress: FfiProgress) {
        self.0.lock().unwrap().push(progress);
    }
}

/// A canonical temporary folder, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "tiny-ffi-worktree-move-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    repo: PathBuf,
    session: Arc<TinySession>,
    trash: FolderTrash,
    /// The listing the user confirmed from.
    shown: FfiWorktreeList,
}

impl Fixture {
    /// A repository with two worktrees, listed through `worktree_list` the
    /// way the app lists them.
    fn listed() -> Self {
        let dir = TempDir::new();
        let root = dir.0.clone();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        assert!(git(&repo, &["init", "-q", "-b", "main"]));
        fs::write(repo.join("a"), "a").unwrap();
        assert!(git(&repo, &["add", "a"]) && git(&repo, &["commit", "-qm", "a"]));
        for name in ["one", "two"] {
            let path = root.join(name);
            let branch = format!("agent/{name}");
            assert!(git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    &branch,
                    path.to_str().unwrap()
                ]
            ));
        }
        let home = root.clone();
        let session = TinySession::new();
        session.worktrees().home = Some(home.clone());
        session.inject_clean_inputs(&home, &home, root.join("rules"));
        let mut agent = std::process::Command::new("/bin/sleep")
            .arg("30")
            .current_dir(&repo)
            .spawn()
            .unwrap();
        let process = FfiAgentProcess {
            pid: agent.id(),
            tool: "claudeCode".into(),
        };
        session.worktree_remember(vec![process]).unwrap();
        agent.kill().unwrap();
        agent.wait().unwrap();
        let shown = session.worktree_list(Vec::new()).unwrap();
        assert_eq!(shown.repos[0].worktrees.len(), 2);
        let trash = FolderTrash(root.join("Trash"));
        fs::create_dir(&trash.0).unwrap();
        Self {
            _dir: dir,
            root,
            repo,
            session,
            trash,
            shown,
        }
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).to_string_lossy().into_owned()
    }

    /// The worktree at `name` as the confirmation showed it.
    fn item(&self, name: &str) -> FfiWorktreeMoveItem {
        let path = self.path(name);
        let shown = self.shown.repos[0]
            .worktrees
            .iter()
            .find(|w| w.path == path)
            .unwrap();
        FfiWorktreeMoveItem {
            path,
            branch: shown.branch.clone(),
            head: shown.head.clone(),
        }
    }

    fn move_items(
        &self,
        items: &[FfiWorktreeMoveItem],
        listener: &Listener,
    ) -> Result<FfiWorktreeMoveReport, FfiError> {
        self.session.move_worktrees(
            items,
            &self.trash,
            &NoProcesses,
            &CancellationToken::default(),
            listener,
        )
    }
}

#[test]
fn only_the_named_listed_worktrees_move_once_and_their_branches_are_free() {
    let fixture = Fixture::listed();
    let listener = Listener::default();
    let one = fixture.path("one");

    let report = fixture
        .move_items(&[fixture.item("one")], &listener)
        .unwrap();

    assert_eq!(
        report.results,
        [FfiWorktreeMoveResult {
            path: one.clone(),
            outcome: FfiWorktreeMoveOutcome::Moved
        }]
    );
    assert!(report.prune_failures.is_empty() && report.stopped.is_none());
    assert!(!Path::new(&one).exists() && fixture.trash.0.join("one").is_dir());
    assert!(
        fixture.root.join("two").is_dir(),
        "an unnamed worktree stays"
    );
    assert!(git(&fixture.repo, &["branch", "-d", "agent/one"]));
    let moved: Vec<String> = listener
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.operation == "worktree-moved")
        .map(|p| p.message.clone())
        .collect();
    assert_eq!(moved, std::slice::from_ref(&one));
    // Spent: the same call is refused, and nothing else moves.
    let again = fixture.move_items(&[fixture.item("one")], &listener);
    assert!(matches!(again, Err(FfiError::PreviewInvalid { .. })));
}

#[test]
fn a_path_the_listing_did_not_show_refuses_the_whole_call() {
    let fixture = Fixture::listed();
    let listener = Listener::default();
    let main_checkout = FfiWorktreeMoveItem {
        path: fixture.repo.to_string_lossy().into_owned(),
        ..fixture.item("one")
    };
    for items in [
        vec![fixture.item("one"), main_checkout],
        vec![fixture.item("one"), fixture.item("one")],
        Vec::new(),
    ] {
        let refused = fixture.move_items(&items, &listener);
        assert!(
            matches!(refused, Err(FfiError::PreviewInvalid { .. })),
            "{items:?}"
        );
    }
    assert!(fixture.root.join("one").is_dir() && fixture.root.join("two").is_dir());
    assert!(listener.0.lock().unwrap().is_empty());
}

#[test]
fn another_write_holding_the_gate_refuses_the_move() {
    let fixture = Fixture::listed();
    let held = fixture.session.begin().unwrap();
    let refused = fixture.move_items(&[fixture.item("one")], &Listener::default());
    assert!(matches!(refused, Err(FfiError::Busy)));
    drop(held);
    assert!(fixture.root.join("one").is_dir());
}

#[test]
fn a_worktree_that_changed_since_the_confirmation_stays_though_a_new_list_shows_it() {
    let fixture = Fixture::listed();
    let listener = Listener::default();
    let confirmed = [fixture.item("one"), fixture.item("two")];
    // While the dialog is open: `one` is on another branch, `two` on another
    // commit (main moved on and `two` followed it), and the page lists again.
    assert!(git(
        &fixture.repo,
        &["branch", "-m", "agent/one", "agent/renamed"]
    ));
    fs::write(fixture.repo.join("b"), "b").unwrap();
    assert!(git(&fixture.repo, &["add", "b"]) && git(&fixture.repo, &["commit", "-qm", "b"]));
    assert!(git(
        &fixture.root.join("two"),
        &["reset", "-q", "--hard", "main"]
    ));
    let relisted = fixture.session.worktree_list(Vec::new()).unwrap();
    let movable = relisted.repos[0].worktrees.iter().all(|w| {
        matches!(
            w.status,
            FfiWorktreeStatus::Merged | FfiWorktreeStatus::LastChange { .. }
        )
    });
    assert!(movable, "both can still be ticked: {relisted:?}");

    let report = fixture.move_items(&confirmed, &listener).unwrap();

    let outcomes: Vec<_> = report.results.iter().map(|r| r.outcome.clone()).collect();
    assert_eq!(
        outcomes,
        [
            FfiWorktreeMoveOutcome::Changed,
            FfiWorktreeMoveOutcome::Changed
        ]
    );
    assert!(fixture.root.join("one").is_dir() && fixture.root.join("two").is_dir());

    // Confirmed again from the new list, they go.
    let shown_now = Fixture {
        shown: relisted,
        ..fixture
    };
    let report = shown_now
        .move_items(&[shown_now.item("one"), shown_now.item("two")], &listener)
        .unwrap();
    assert!(report
        .results
        .iter()
        .all(|r| r.outcome == FfiWorktreeMoveOutcome::Moved));
}
