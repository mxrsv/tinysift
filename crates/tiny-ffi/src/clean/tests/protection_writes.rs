//! Keeping and un-keeping through Rust: typed errors, stale revisions, other
//! sessions, changed stores, failed saves and the stored preview. Temp stores,
//! temp dirs and `FakeTrash` only.

use std::os::unix::fs::PermissionsExt;

use super::*;

fn providers(dir: &Path) -> Vec<Box<dyn CleanProvider>> {
    vec![Box::new(Fixture::new("logs", dir))]
}

/// Restores a directory's mode on drop, so a failed assertion cannot leave a
/// read-only folder behind.
struct RestoreMode(PathBuf);

impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

fn run(
    session: &TestSession,
    preview_id: &str,
    dir: &Path,
    trash: &FakeTrash,
) -> Result<FfiExecReport, FfiError> {
    execute(
        session,
        preview_id,
        dir,
        trash,
        &CancellationToken::default(),
    )
}

#[test]
fn unknown_stale_and_unloaded_requests_are_rejected_without_writing() {
    let dir = fixture_dir("ids", &["a"], 1);
    let session = TestSession::new();
    let first = discover(&session, &providers(&dir)).unwrap();
    let old = id_of(&first, "/a");

    // Never loaded: a write cannot name a revision it never saw.
    let unloaded = session.clean_keep(old.clone(), 0);
    assert!(
        matches!(unloaded, Err(FfiError::ProtectionChanged)),
        "{unloaded:?}"
    );

    let second = discover(&session, &providers(&dir)).unwrap();
    session.clean_protection().unwrap();
    let stale = session.clean_keep(old, 0);
    assert!(
        matches!(stale, Err(FfiError::PreviewInvalid { .. })),
        "{stale:?}"
    );
    let forged = session.clean_keep("d9-9".into(), 0);
    assert!(
        matches!(forged, Err(FfiError::PreviewInvalid { .. })),
        "{forged:?}"
    );
    let unknown_rule = session.clean_unkeep("r7".into(), 0);
    assert!(
        matches!(unknown_rule, Err(FfiError::InvalidInput { .. })),
        "{unknown_rule:?}"
    );

    let kept = keep_candidate(&session, &id_of(&second, "/a"));
    assert_eq!((revision(&kept), rules(&kept).len()), (1, 1));
    let again = session.clean_keep(id_of(&second, "/a"), 1);
    assert!(
        matches!(&again, Err(FfiError::InvalidInput { detail }) if detail.contains("r1")),
        "{again:?}"
    );
    remove(&dir);
}

#[test]
fn rules_survive_a_relaunch_and_removal_selects_nothing() {
    let dir = fixture_dir("relaunch", &["a", "b"], 3);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let kept = keep_candidate(&session, &id_of(&discovery, "/a"));
    let saved = rules(&kept);
    assert_eq!(saved[0].path, dir.join("a").to_string_lossy());
    assert_eq!(saved[0].scope, FfiProtectionScope::File);
    assert!(saved[0].path_exists);
    assert!(session.store().dir().join("initialized").exists());

    let relaunched = session.sibling();
    let discovery = discover(&relaunched, &providers(&dir)).unwrap();
    let loaded = relaunched.clean_protection().unwrap();
    assert_eq!((revision(&loaded), rules(&loaded)), (1, saved));
    assert!(!candidate_protection(&loaded, &discovery, "/a")
        .guards
        .is_empty());

    let freed = relaunched.clean_unkeep("r1".into(), 1).unwrap();
    assert_eq!((revision(&freed), rules(&freed).len()), (2, 0));
    assert!(candidate_protection(&freed, &discovery, "/a")
        .guards
        .is_empty());
    // The freed path is not selected for anyone: a preview holds only what was asked.
    let preview = relaunched
        .clean_preview(vec![id_of(&discovery, "/b")])
        .unwrap();
    assert_eq!(preview.items.len(), 1);
    remove(&dir);
}

#[test]
fn a_rule_write_drops_the_stored_preview_and_a_failed_validation_keeps_it() {
    let dir = fixture_dir("drop-preview", &["a", "b"], 3);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let preview = session
        .clean_preview(vec![id_of(&discovery, "/a")])
        .unwrap();
    session.clean_protection().unwrap();
    let rejected = session.clean_keep("d9-9".into(), 0);
    assert!(rejected.is_err());
    assert!(
        session.clean_state().preview.is_some(),
        "an unknown ID writes nothing"
    );

    keep_candidate(&session, &id_of(&discovery, "/b"));
    let trash = FakeTrash::default();
    let result = run(&session, &preview.preview_id, &dir, &trash);
    assert!(
        matches!(result, Err(FfiError::PreviewInvalid { .. })),
        "{result:?}"
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    remove(&dir);
}

#[test]
fn another_session_or_an_old_revision_cannot_be_overwritten() {
    let dir = fixture_dir("two-sessions", &["a", "b", "c"], 1);
    let (first, second) = {
        let first = TestSession::new();
        let second = first.sibling();
        (first, second)
    };
    let mine = discover(&first, &providers(&dir)).unwrap();
    let theirs = discover(&second, &providers(&dir)).unwrap();
    assert_eq!(revision(&first.clean_protection().unwrap()), 0);

    keep_candidate(&second, &id_of(&theirs, "/b"));
    let blocked = first.clean_keep(id_of(&mine, "/a"), 0);
    assert!(
        matches!(blocked, Err(FfiError::ProtectionChanged)),
        "{blocked:?}"
    );
    // The failed write forgot what it had seen: reload before any other write.
    let unloaded = first.clean_keep(id_of(&mine, "/a"), 1);
    assert!(
        matches!(unloaded, Err(FfiError::ProtectionChanged)),
        "{unloaded:?}"
    );

    let reloaded = first.clean_protection().unwrap();
    assert_eq!(revision(&reloaded), 1);
    let old = first.clean_keep(id_of(&mine, "/a"), 0);
    assert!(matches!(old, Err(FfiError::ProtectionChanged)), "{old:?}");
    let current = keep_candidate(&first, &id_of(&mine, "/a"));
    assert_eq!((revision(&current), rules(&current).len()), (2, 2));
    // Nothing the other session saved was lost.
    let listed: Vec<_> = rules(&second.clean_protection().unwrap())
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(listed, ["r1", "r2"]);
    remove(&dir);
}

#[test]
fn an_edit_that_keeps_the_revision_is_still_a_change() {
    let dir = fixture_dir("same-revision", &["a", "b"], 2);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    keep_candidate(&session, &id_of(&discovery, "/a"));
    let preview = session
        .clean_preview(vec![id_of(&discovery, "/b")])
        .unwrap();
    session.clean_protection().unwrap();

    // Another process rewrites the file with the same revision and different content.
    let rules_file = session.store().dir().join("rules.json");
    let other = dir.join("elsewhere");
    let edited = format!(
        r#"{{"version":1,"revision":1,"next_id":2,"rules":[{{"id":"r1","path":{:?},"scope":"file"}}]}}"#,
        other.to_string_lossy()
    );
    std::fs::write(&rules_file, edited).unwrap();

    let write = session.clean_keep(id_of(&discovery, "/b"), 1);
    assert!(
        matches!(write, Err(FfiError::ProtectionChanged)),
        "{write:?}"
    );
    let trash = FakeTrash::default();
    let stale = run(&session, &preview.preview_id, &dir, &trash);
    assert!(
        matches!(stale, Err(FfiError::ProtectionChanged)),
        "{stale:?}"
    );
    let replay = run(&session, &preview.preview_id, &dir, &trash);
    assert!(
        matches!(replay, Err(FfiError::PreviewInvalid { .. })),
        "a preview built for other rules is dropped, not kept for a retry"
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    remove(&dir);
}

#[test]
fn rules_that_appear_after_a_preview_stop_it_before_it_is_used() {
    let dir = fixture_dir("appeared", &["a", "b"], 2);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let preview = session.clean_preview(ids(&discovery)).unwrap();
    assert!(!session.dir.exists(), "no store existed at preview time");

    let other = session.sibling();
    let theirs = discover(&other, &providers(&dir)).unwrap();
    keep_candidate(&other, &id_of(&theirs, "/b"));

    let trash = FakeTrash::default();
    let blocked = run(&session, &preview.preview_id, &dir, &trash);
    assert!(
        matches!(blocked, Err(FfiError::ProtectionChanged)),
        "{blocked:?}"
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    assert!(dir.join("a").exists() && dir.join("b").exists());

    // A new preview sees the rule and moves only what it allows.
    let fresh = session.clean_preview(ids(&discovery)).unwrap();
    let report = run(&session, &fresh.preview_id, &dir, &trash).unwrap();
    assert_eq!(report.moved_count, 1);
    assert_eq!(trash.moved.lock().unwrap().as_slice(), [dir.join("a")]);
    remove(&dir);
}

#[test]
fn contention_is_busy_for_reads_and_writes() {
    let dir = fixture_dir("busy-store", &["a", "b"], 1);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    keep_candidate(&session, &id_of(&discovery, "/a"));
    let loaded = session.clean_protection().unwrap();

    {
        let _gate = session.begin().unwrap();
        let busy = session.clean_keep(id_of(&discovery, "/b"), revision(&loaded));
        assert!(matches!(busy, Err(FfiError::Busy)), "{busy:?}");
    }
    let store = session.store();
    let holder = store.lease().unwrap();
    for result in [
        session.clean_keep(id_of(&discovery, "/b"), 1).map(drop),
        session.clean_unkeep("r1".into(), 1).map(drop),
        session.clean_protection().map(drop),
        session
            .clean_preview(vec![id_of(&discovery, "/b")])
            .map(drop),
    ] {
        assert!(matches!(result, Err(FfiError::Busy)), "{result:?}");
    }
    assert!(!session.is_busy());
    drop(holder);
    assert!(session.clean_keep(id_of(&discovery, "/b"), 1).is_ok());
    remove(&dir);
}

#[test]
fn a_failed_save_leaves_the_previous_rules_and_says_so() {
    let dir = fixture_dir("save-fails", &["a", "b"], 1);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    keep_candidate(&session, &id_of(&discovery, "/a"));
    session.clean_protection().unwrap();

    let store_dir = session.store().dir().to_path_buf();
    let _restore = RestoreMode(store_dir.clone());
    std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = session.clean_keep(id_of(&discovery, "/b"), 1);
    assert!(
        matches!(failed, Err(FfiError::ProtectionNotSaved { .. })),
        "{failed:?}"
    );
    std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let reloaded = session.clean_protection().unwrap();
    assert_eq!((revision(&reloaded), rules(&reloaded).len()), (1, 1));
    let retried = session.clean_keep(id_of(&discovery, "/b"), 1).unwrap();
    assert_eq!(revision(&retried), 2);
    remove(&dir);
}

#[test]
fn an_unreadable_store_blocks_every_write_until_it_reads_again() {
    let dir = fixture_dir("unreadable-write", &["a", "b"], 1);
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    keep_candidate(&session, &id_of(&discovery, "/a"));
    let rules_file = session.store().dir().join("rules.json");
    let saved = std::fs::read(&rules_file).unwrap();
    std::fs::write(&rules_file, b"{ not json").unwrap();

    let keep = session.clean_keep(id_of(&discovery, "/b"), 1);
    assert!(
        matches!(keep, Err(FfiError::ProtectionUnavailable { .. })),
        "{keep:?}"
    );
    let unkeep = session.clean_unkeep("r1".into(), 1);
    assert!(
        matches!(unkeep, Err(FfiError::ProtectionUnavailable { .. })),
        "{unkeep:?}"
    );

    std::fs::write(&rules_file, saved).unwrap();
    let back = session.clean_protection().unwrap();
    assert_eq!((revision(&back), rules(&back).len()), (1, 1));
    remove(&dir);
}

#[test]
fn a_kept_path_must_still_be_what_the_user_saw() {
    let dir = fixture_dir("revalidate", &["a", "b"], 1);
    std::os::unix::fs::symlink(dir.join("b"), dir.join("link")).unwrap();
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    session.clean_protection().unwrap();

    std::fs::remove_file(dir.join("a")).unwrap();
    std::fs::create_dir(dir.join("a")).unwrap();
    let swapped = session.clean_keep(id_of(&discovery, "/a"), 0);
    assert!(
        matches!(&swapped, Err(FfiError::InvalidInput { detail }) if detail.contains("changed kind")),
        "a file replaced by a folder would widen the rule: {swapped:?}"
    );
    let link = session.clean_keep(id_of(&discovery, "/link"), 0);
    assert!(
        matches!(link, Err(FfiError::InvalidInput { .. })),
        "{link:?}"
    );
    assert!(!session.store().dir().exists(), "nothing was written");
    remove(&dir);
}
