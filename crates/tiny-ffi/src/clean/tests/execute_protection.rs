//! Saved "always keep" rules at execution: the FFI maps core refusals to
//! typed records and never spends a preview on a busy or unreadable store.
//! Temp stores and `FakeTrash` only.

use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::protection::RuleScope;

use super::*;

/// Saves `path` as a rule, as an earlier session did.
fn keep(session: &TestSession, path: &Path, scope: RuleScope) {
    let store = session.store();
    let mut lease = store.lease().unwrap();
    let revision = lease.snapshot().revision;
    lease.add(revision, path, scope).unwrap();
}

fn run(
    session: &TestSession,
    preview_id: &str,
    root: &Path,
    trash: &dyn Trash,
) -> Result<FfiExecReport, FfiError> {
    let root = root.to_path_buf();
    let providers = move |_: &CleanOptions| -> Vec<Box<dyn CleanProvider>> {
        vec![Box::new(Fixture::new("logs", &root))]
    };
    session.execute_clean(
        preview_id,
        &providers,
        &NotRunning,
        trash,
        &CancellationToken::default(),
        &Progress::default(),
    )
}

/// The outcome of the item whose path ends in `name`.
fn outcome_of(report: &FfiExecReport, name: &str) -> FfiItemOutcome {
    let result = report.results.iter().find(|r| r.path.ends_with(name));
    result
        .unwrap_or_else(|| panic!("no result for {name}"))
        .outcome
        .clone()
}

/// Removes or corrupts the saved rules after the first move.
struct SabotagingTrash {
    inner: FakeTrash,
    store: ProtectionStore,
    corrupt: bool,
}

impl Trash for SabotagingTrash {
    fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
        self.inner.move_to_trash(path)?;
        match self.corrupt {
            true => std::fs::write(self.store.dir().join("rules.json"), b"{ not json").unwrap(),
            false => std::fs::remove_dir_all(self.store.dir()).unwrap(),
        }
        Ok(())
    }
}

#[test]
fn kept_paths_are_skipped_with_a_typed_reason_and_never_trashed() {
    let dir = fixture_dir("kept", &["a", "b", "c"], 4);
    std::fs::create_dir(dir.join("d")).unwrap();
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    keep(&session, &dir.join("b"), RuleScope::File);
    // Discovery lists `d`, never `d/inner/precious`; the rule still holds `d`.
    keep(&session, &dir.join("d/inner/precious"), RuleScope::File);
    // Defense in depth: previews already hold these paths back, so forge a
    // preview authorized against the new rules, as if a bug let them through.
    let rules = session.store().load().unwrap();
    session.clean_state().preview.as_mut().unwrap().policy = rules;
    let trash = FakeTrash::default();
    let report = run(&session, &preview.preview_id, &dir, &trash).unwrap();
    assert_eq!(outcome_of(&report, "/a"), FfiItemOutcome::MovedToTrash);
    assert_eq!(outcome_of(&report, "/c"), FfiItemOutcome::MovedToTrash);
    assert_eq!(
        outcome_of(&report, "/b"),
        FfiItemOutcome::Skipped {
            reason: FfiSkipReason::Kept,
            detail: Some(dir.join("b").to_string_lossy().into_owned())
        }
    );
    assert_eq!(
        outcome_of(&report, "/d"),
        FfiItemOutcome::Skipped {
            reason: FfiSkipReason::HoldsKept,
            detail: Some(dir.join("d/inner/precious").to_string_lossy().into_owned())
        }
    );
    assert_eq!((report.moved_count, report.skipped_count), (2, 2));
    assert_eq!(report.stopped, None);
    let mut moved = trash.moved.lock().unwrap().clone();
    moved.sort();
    assert_eq!(moved, [dir.join("a"), dir.join("c")]);
    remove(&dir);
}

#[test]
fn a_busy_store_is_busy_and_does_not_spend_the_preview() {
    let dir = fixture_dir("busy", &["a"], 4);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let store = session.store();
    let holder = store.lease().unwrap();
    let trash = FakeTrash::default();
    let blocked = run(&session, &preview.preview_id, &dir, &trash);
    assert!(matches!(blocked, Err(FfiError::Busy)), "{blocked:?}");
    assert!(!session.is_busy());
    assert!(trash.moved.lock().unwrap().is_empty());
    drop(holder);
    let report = run(&session, &preview.preview_id, &dir, &trash).unwrap();
    assert_eq!(report.moved_count, 1);
    remove(&dir);
}

#[test]
fn an_unreadable_store_blocks_execution_without_spending_the_preview() {
    let dir = fixture_dir("unreadable", &["a"], 4);
    let session = TestSession::new();
    keep(&session, &dir.join("elsewhere"), RuleScope::File);
    let preview = preview_all(&session, &dir);
    let rules = session.store().dir().join("rules.json");
    let saved = std::fs::read(&rules).unwrap();
    std::fs::write(&rules, b"{ not json").unwrap();
    let trash = FakeTrash::default();
    let blocked = run(&session, &preview.preview_id, &dir, &trash);
    assert!(
        matches!(&blocked, Err(FfiError::ProtectionUnavailable { detail }) if detail.contains("malformed")),
        "{blocked:?}"
    );
    assert!(!session.is_busy());
    assert!(trash.moved.lock().unwrap().is_empty());
    // Not reset or ignored: only rules that read again, unchanged, let the same preview run.
    std::fs::write(&rules, saved).unwrap();
    assert_eq!(
        run(&session, &preview.preview_id, &dir, &trash)
            .unwrap()
            .moved_count,
        1
    );
    remove(&dir);
}

#[test]
fn rules_that_change_mid_run_stop_it_with_a_typed_reason() {
    for corrupt in [false, true] {
        let dir = fixture_dir("changed", &["a", "b", "c"], 4);
        let session = TestSession::new();
        keep(&session, &dir.join("elsewhere"), RuleScope::File);
        let preview = preview_all(&session, &dir);
        let trash = SabotagingTrash {
            inner: FakeTrash::default(),
            store: session.store(),
            corrupt,
        };
        let report = run(&session, &preview.preview_id, &dir, &trash).unwrap();
        match (corrupt, &report.stopped) {
            (false, Some(FfiStopReason::ProtectionChanged)) => {}
            (true, Some(FfiStopReason::ProtectionUnavailable { detail })) => {
                assert!(detail.contains("malformed"), "{detail}")
            }
            (_, other) => panic!("corrupt={corrupt}: {other:?}"),
        }
        assert_eq!(
            (report.moved_count, report.not_attempted_count),
            (1, 2),
            "later items are never retried"
        );
        assert_eq!(trash.inner.moved.lock().unwrap().len(), 1);
        remove(&dir);
    }
}

#[test]
#[should_panic(expected = "protection_dir")]
fn tests_cannot_fall_back_to_the_real_store() {
    TinySession::new().protection_store(Path::new("/nonexistent"));
}
