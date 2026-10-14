use std::os::unix::fs::{symlink, PermissionsExt};

use super::super::protection::{Matcher, Relation};
use super::*;

macro_rules! assert_err {
    ($result:expr, $pattern:pat $(if $guard:expr)?) => {
        match $result {
            Err($pattern) $(if $guard)? => {}
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("expected an error"),
        }
    };
}

fn setup() -> (tempfile::TempDir, ProtectionStore, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    fs::create_dir_all(data.join("keep/inner")).unwrap();
    fs::create_dir_all(data.join("other")).unwrap();
    let store = ProtectionStore::at(tmp.path().join("Tiny").join(STORE_DIR));
    (tmp, store, data)
}

fn save(store: &ProtectionStore, path: &Path) -> Result<ProtectionRule, StoreError> {
    let mut lease = store.lease()?;
    let revision = lease.snapshot().revision;
    lease.add(revision, path, RuleScope::Directory)
}

fn relation(snapshot: &PolicySnapshot, candidate: &Path) -> Option<Relation> {
    let eval = snapshot.evaluate(&Matcher::new(), candidate);
    assert!(eval.uncertain.is_empty(), "{:?}", eval.uncertain);
    eval.blocks.first().map(|b| b.relation)
}

fn reopen(store: &ProtectionStore) -> ProtectionStore {
    ProtectionStore::at(store.dir().to_path_buf())
}

fn rules_file(store: &ProtectionStore) -> PathBuf {
    store.dir().join(RULES_FILE)
}

#[test]
fn rules_survive_reload_and_removal_returns_the_path_to_cleanup() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ProtectionStore>();
    assert_send_sync::<PolicySnapshot>();
    let (_tmp, store, data) = setup();
    let keep = data.join("keep");
    let absent = store.load().unwrap();
    assert!(!absent.stored && absent.revision == 0 && absent.rules.is_empty());
    assert!(!store.dir().exists() && !store.lock_path().exists());

    assert_eq!(save(&store, &keep).unwrap().id, "r1");
    let reloaded = reopen(&store).load().unwrap();
    assert_eq!((reloaded.revision, reloaded.rules.len()), (1, 1));
    let inner = keep.join("inner");
    assert_eq!(relation(&reloaded, &inner), Some(Relation::InsideRule));
    assert_eq!(relation(&reloaded, &data.join("other")), None);
    let mode = |path: &Path| fs::metadata(path).unwrap().mode() & 0o777;
    assert_eq!(
        (mode(store.dir()), mode(&rules_file(&store))),
        (0o700, 0o600)
    );

    assert_eq!(save(&store, &data.join("other")).unwrap().id, "r2");
    let mut lease = store.lease().unwrap();
    lease.remove(2, "r1").unwrap();
    lease.remove(3, "r2").unwrap();
    drop(lease);
    let emptied = store.load().unwrap();
    assert!(emptied.stored && emptied.rules.is_empty() && emptied.revision == 4);
    assert!(store.dir().join(MARKER_FILE).exists(), "the marker stays");
    assert_eq!(save(&store, &keep).unwrap().id, "r3", "ids are not reused");
    let tiny = Path::new("/Users/me/Library/Application Support/Tiny");
    let home = ProtectionStore::for_home(Path::new("/Users/me"));
    assert_eq!(home.dir(), tiny.join(STORE_DIR));
}

#[test]
fn writes_check_revision_duplicates_and_unknown_ids() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    let mut lease = store.lease().unwrap();
    let other = data.join("other");
    let stale = lease.add(0, &other, RuleScope::File);
    assert_err!(stale, StoreError::Stale { actual: 1, .. });
    let dup = lease.add(1, &data.join("keep"), RuleScope::File);
    assert_err!(dup, StoreError::Duplicate(id) if id == "r1");
    assert_err!(lease.remove(1, "r9"), StoreError::UnknownRule(_));
}

#[test]
fn the_store_and_its_lock_are_protected_from_cleanup() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    let snapshot = store.load().unwrap();
    let (lock, rules) = (store.lock_path(), rules_file(&store));
    for protected in [store.dir(), &rules, &lock, store.dir().parent().unwrap()] {
        let eval = snapshot.evaluate(&Matcher::new(), protected);
        assert!(!eval.allows_move(), "{protected:?}");
        let internal = eval
            .blocks
            .iter()
            .all(|b| b.rule_id.starts_with("internal:"));
        assert!(internal);
    }
    let other = data.join("other");
    assert!(snapshot.evaluate(&Matcher::new(), &other).allows_move());
}

#[test]
fn malformed_unknown_version_and_insecure_storage_fail_closed() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    let rules = rules_file(&store);
    let good = fs::read(&rules).unwrap();
    let load_with = |text: &str| {
        fs::write(&rules, text).unwrap();
        store.load()
    };
    assert_err!(load_with("{ not json"), StoreError::Malformed(_));
    let v2 = r#"{"version":2,"revision":1,"next_id":2,"rules":[]}"#;
    assert_err!(
        load_with(v2),
        StoreError::Invalid(RuleError::UnsupportedVersion(_))
    );
    fs::write(&rules, good).unwrap();
    assert!(store.load().is_ok());

    let chmod = |path: &Path, mode| fs::set_permissions(path, fs::Permissions::from_mode(mode));
    chmod(&rules, 0o644).unwrap();
    assert_err!(store.load(), StoreError::UnsafeStorage { .. });
}

#[test]
fn a_missing_rules_file_or_marker_stays_an_error_after_relaunch() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    fs::remove_file(rules_file(&store)).unwrap();
    let relaunched = reopen(&store);
    assert_err!(relaunched.load(), StoreError::Incomplete(_));
    assert_err!(relaunched.lease(), StoreError::Incomplete(_));
    assert!(!rules_file(&store).exists(), "nothing was recreated");
}

#[test]
fn a_symlinked_store_lock_or_parent_is_rejected() {
    let (tmp, store, data) = setup();
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let parent = store.dir().parent().unwrap().to_path_buf();
    fs::create_dir_all(&parent).unwrap();
    symlink(&elsewhere, store.dir()).unwrap();
    assert_err!(store.load(), StoreError::UnsafeStorage { .. });
    fs::remove_file(store.dir()).unwrap();

    symlink(elsewhere.join("lock"), store.lock_path()).unwrap();
    assert!(store.lease().is_err());
    assert!(!elsewhere.join("lock").exists(), "not followed");
    fs::remove_file(store.lock_path()).unwrap();

    fs::remove_dir(&parent).unwrap();
    symlink(&elsewhere, &parent).unwrap();
    assert_err!(save(&store, &data), StoreError::UnsafeStorage { .. });
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
}

#[test]
fn the_lock_serializes_separate_handles_and_unwinds() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    let other = reopen(&store);
    let lease = store.lease().unwrap();
    assert_err!(other.lease(), StoreError::Busy);
    assert_err!(other.load(), StoreError::Busy);
    drop(lease);
    let unwinding = || {
        let _held = store.lease().unwrap();
        panic!("while locked");
    };
    assert!(std::panic::catch_unwind(unwinding).is_err());
    other.lease().expect("unwinding frees the lock");
}

#[test]
fn concurrent_writers_each_land_one_rule() {
    let (_tmp, store, data) = setup();
    let writers = 8;
    thread::scope(|scope| {
        for n in 0..writers {
            let handle = reopen(&store);
            let path = data.join(format!("rule-{n}"));
            scope.spawn(move || loop {
                match save(&handle, &path) {
                    Err(StoreError::Busy) => thread::yield_now(),
                    other => break drop(other.unwrap()),
                }
            });
        }
    });
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.revision, writers);
    assert_eq!(snapshot.rules.len(), writers as usize, "unique ids load");
}

#[test]
fn external_edits_and_store_loss_are_detected_never_treated_as_first_use() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    let before = store.load().unwrap();
    let edit = |from: &str, to: &str| {
        let rules = rules_file(&store);
        let text = fs::read_to_string(&rules).unwrap().replace(from, to);
        fs::write(rules, text).unwrap();
    };
    edit("keep", "other");
    let after = store.load().unwrap();
    assert_eq!(after.revision, before.revision);
    assert_ne!(after, before, "content differs at the same revision");
    let mut lease = store.lease().unwrap();
    edit("other", "keep");
    assert_err!(lease.ensure_unchanged(), StoreError::Changed);
    let other = data.join("other");
    assert_err!(lease.add(1, &other, RuleScope::File), StoreError::Changed);

    lease.reload().unwrap();
    let before_loss = lease.snapshot().clone();
    fs::remove_dir_all(store.dir()).unwrap();
    assert_err!(lease.ensure_unchanged(), StoreError::StoreLost);
    assert_err!(lease.add(1, &other, RuleScope::File), StoreError::StoreLost);
    assert!(!store.dir().exists(), "no bootstrap over a vanished store");
    assert_err!(lease.reload(), StoreError::StoreLost);
    assert_eq!(
        lease.snapshot(),
        &before_loss,
        "a failed reload preserves the whole policy"
    );
}

#[test]
fn a_failed_update_keeps_the_previous_rules_at_every_step() {
    let (_tmp, store, data) = setup();
    save(&store, &data.join("keep")).unwrap();
    let other = data.join("other");
    for fault in [Fault::BeforeWrite, Fault::BeforeSync, Fault::BeforeRename] {
        let mut lease = store.lease().unwrap();
        store.fail_at(Some(fault));
        let failed = lease.add(1, &other, RuleScope::File);
        store.fail_at(None);
        assert_err!(failed, StoreError::NotSaved(_));
        assert_eq!(lease.reload().unwrap().rules.len(), 1, "{fault:?}");
        assert!(!store.dir().join(RULES_TEMP).exists(), "temp file left");
    }
    let mut lease = store.lease().unwrap();
    store.fail_at(Some(Fault::AfterRename));
    let unsure = lease.add(1, &other, RuleScope::File);
    store.fail_at(None);
    assert_err!(unsure, StoreError::CommitUncertain(_));
    assert_eq!(lease.snapshot().revision, 1, "uncertain is not success");
    assert_err!(lease.ensure_unchanged(), StoreError::Changed);
    assert_eq!(lease.reload().unwrap().rules.len(), 2, "the rename landed");
}

#[test]
fn first_save_failures_before_and_after_publication_recover_by_relaunch() {
    for fault in [Fault::BeforeWrite, Fault::BeforeSync, Fault::BeforePublish] {
        let (_tmp, store, data) = setup();
        store.fail_at(Some(fault));
        assert_err!(save(&store, &data.join("keep")), StoreError::NotSaved(_));
        assert!(!store.dir().exists(), "{fault:?}: nothing was published");

        let relaunched = reopen(&store);
        let loaded = relaunched.load().unwrap();
        assert!(!loaded.stored && loaded.rules.is_empty(), "no policy");
        assert_eq!(save(&relaunched, &data.join("keep")).unwrap().id, "r1");
        assert_eq!(relaunched.load().unwrap().rules.len(), 1);
    }
    let (_tmp, store, data) = setup();
    let (keep, other) = (data.join("keep"), data.join("other"));
    let mut held = store.lease().unwrap();
    store.fail_at(Some(Fault::AfterPublish));
    let first = held.add(0, &keep, RuleScope::Directory);
    store.fail_at(None);
    assert_err!(first, StoreError::CommitUncertain(_));
    // Retrying on the same lease sees the published store, not first use.
    let again = held.add(0, &keep, RuleScope::Directory);
    assert_err!(again, StoreError::Changed);
    drop(held);
    let relaunched = reopen(&store);
    assert_eq!(save(&relaunched, &other).unwrap().id, "r2");
    let after = relaunched.load().unwrap();
    assert_eq!((after.revision, after.rules[0].id.as_str()), (2, "r1"));
}

#[test]
fn staging_never_resets_a_committed_store_and_foreign_staging_is_kept() {
    let (_tmp, store, data) = setup();
    fs::create_dir_all(store.staging_path()).unwrap();
    fs::write(store.staging_path().join("notes.txt"), b"mine").unwrap();
    let blocked = save(&store, &data.join("keep"));
    assert_err!(blocked, StoreError::UnsafeStorage { .. });
    let notes = store.staging_path().join("notes.txt");
    assert!(notes.exists() && !store.dir().exists());
    fs::remove_file(notes).unwrap();

    save(&store, &data.join("keep")).unwrap();
    let committed = store.load().unwrap();
    // Staging from an older interrupted first save is never copied over,
    // or used to rebuild, the committed store.
    fs::create_dir_all(store.staging_path()).unwrap();
    fs::write(store.staging_path().join(RULES_FILE), b"{}").unwrap();
    assert_eq!(store.load().unwrap(), committed);
    let mut lease = store.lease().unwrap();
    lease.add(1, &data.join("other"), RuleScope::File).unwrap();
    drop(lease);
    let after = store.load().unwrap();
    assert_eq!((after.revision, &after.rules[0]), (2, &committed.rules[0]));
    let leftover = fs::read(store.staging_path().join(RULES_FILE)).unwrap();
    assert_eq!(leftover, b"{}", "an update never touches staging");
}
