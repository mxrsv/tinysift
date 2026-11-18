//! The consent contract of `worktree_move` on git fixtures in a temporary
//! folder: only listed worktrees move, once, behind the gate. The Trash is a
//! folder of the fixture, never the user's.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
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
        let listed = session.worktree_list(Vec::new()).unwrap();
        assert_eq!(listed.repos[0].worktrees.len(), 2);
        let trash = FolderTrash(root.join("Trash"));
        fs::create_dir(&trash.0).unwrap();
        Self {
            _dir: dir,
            root,
            repo,
            session,
            trash,
        }
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).to_string_lossy().into_owned()
    }

    fn move_paths(
        &self,
        paths: &[String],
        listener: &Listener,
    ) -> Result<FfiWorktreeMoveReport, FfiError> {
        self.session.move_worktrees(
            paths,
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
        .move_paths(std::slice::from_ref(&one), &listener)
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
    let again = fixture.move_paths(&[one], &listener);
    assert!(matches!(again, Err(FfiError::PreviewInvalid { .. })));
}

#[test]
fn a_path_the_listing_did_not_show_refuses_the_whole_call() {
    let fixture = Fixture::listed();
    let listener = Listener::default();
    let main_checkout = fixture.repo.to_string_lossy().into_owned();
    for paths in [
        vec![fixture.path("one"), main_checkout],
        vec![fixture.path("one"), fixture.path("one")],
        Vec::new(),
    ] {
        let refused = fixture.move_paths(&paths, &listener);
        assert!(
            matches!(refused, Err(FfiError::PreviewInvalid { .. })),
            "{paths:?}"
        );
    }
    assert!(fixture.root.join("one").is_dir() && fixture.root.join("two").is_dir());
    assert!(listener.0.lock().unwrap().is_empty());
}

#[test]
fn another_write_holding_the_gate_refuses_the_move() {
    let fixture = Fixture::listed();
    let held = fixture.session.begin().unwrap();
    let refused = fixture.move_paths(&[fixture.path("one")], &Listener::default());
    assert!(matches!(refused, Err(FfiError::Busy)));
    drop(held);
    assert!(fixture.root.join("one").is_dir());
}
