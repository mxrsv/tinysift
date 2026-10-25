use super::*;

#[test]
fn permanent_uninstall_uses_the_same_running_protection_and_identity_guards() {
    struct RecordingDelete(Mutex<Vec<PathBuf>>);
    impl Delete for RecordingDelete {
        fn delete(&self, path: &Path) -> std::io::Result<()> {
            self.0.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }
    let world = World::new();
    world.keep(&world.path(DATA), RuleScope::Directory);
    let lease = world.store.lease().unwrap();
    let sink = RecordingDelete(Mutex::default());
    let trash = TestTrash::new();
    let ctx = UninstallContext {
        home: &world.home,
        probe: &MockChecker::none(),
        trash: &trash,
        cancel: None,
        progress: None,
        policy: &lease,
    };
    let report = execute_checked_uninstall_action(
        &world.plan,
        &BTreeSet::from([BUNDLE, CACHE, DATA]),
        &ctx,
        Some(&sink),
    )
    .unwrap();
    assert_eq!(
        *sink.0.lock().unwrap(),
        [world.path(BUNDLE), world.path(CACHE)]
    );
    assert!(matches!(report.results[2].outcome, ItemOutcome::Skipped(_)));
    assert!(trash.moved().is_empty());
    let running = MockChecker::with_running([EXE]);
    let running_ctx = UninstallContext {
        probe: &running,
        ..ctx
    };
    assert!(execute_checked_uninstall_action(
        &world.plan,
        &BTreeSet::from([CACHE]),
        &running_ctx,
        Some(&sink)
    )
    .is_err());
    assert_eq!(sink.0.lock().unwrap().len(), 2);
}
#[test]
fn leftovers_only_rejects_app_replacement_since_the_plan_was_frozen() {
    let world = World::new();
    let bundle = world.path(BUNDLE);
    fs::rename(&bundle, world.apps.join("Original.app")).unwrap();
    make_dir(&bundle);
    let trash = TestTrash::new();
    assert!(world
        .run_with(&[CACHE], &MockChecker::none(), &trash, None)
        .is_err());
    assert!(trash.moved().is_empty());
}

#[test]
fn leftovers_only_rechecks_the_app_identity_after_the_running_probe() {
    let world = World::new();
    let bundle = world.path(BUNDLE);
    let original = world.apps.join("Original.app");
    let probe = ScriptedProbe {
        calls: AtomicUsize::new(0),
        running_from: usize::MAX,
        on_call: Box::new(move |call| {
            if call == 1 {
                fs::rename(&bundle, &original).unwrap();
                make_dir(&bundle);
            }
        }),
    };
    let trash = TestTrash::new();
    let report = world.run_with(&[CACHE], &probe, &trash, None).unwrap();
    assert!(matches!(
        report.results[0].outcome,
        ItemOutcome::Skipped(SkipReason::Changed)
    ));
    assert!(trash.moved().is_empty());
}

struct DeleteSink {
    calls: Mutex<Vec<PathBuf>>,
    fail_on: Option<PathBuf>,
}
impl Delete for DeleteSink {
    fn delete(&self, path: &Path) -> std::io::Result<()> {
        self.calls.lock().unwrap().push(path.to_path_buf());
        if self.fail_on.as_deref() == Some(path) {
            Err(std::io::Error::other("fixture deletion refused"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn cli_wrapper_applies_consent_to_both_real_execution_routes() {
    for permanent in [false, true] {
        for consent in [Consent::Unattended, Consent::Explicit] {
            let world = World::new();
            let lease = world.store.lease().unwrap();
            let trash = TestTrash::new();
            let delete = DeleteSink {
                calls: Mutex::default(),
                fail_on: None,
            };
            let ctx = UninstallContext {
                home: &world.home,
                probe: &MockChecker::none(),
                trash: &trash,
                cancel: None,
                progress: None,
                policy: &lease,
            };
            let report = crate::uninstall::execute_prepared_with(
                &world.plan,
                consent,
                &ctx,
                permanent.then_some(&delete as &dyn Delete),
            )
            .unwrap();
            let expected = if consent == Consent::Unattended {
                vec![BUNDLE, CACHE]
            } else {
                vec![BUNDLE, CACHE, DATA]
            };
            assert_eq!(
                report
                    .results
                    .iter()
                    .map(|item| item.index)
                    .collect::<Vec<_>>(),
                expected
            );
            let expected_paths: Vec<_> = expected.iter().map(|&index| world.path(index)).collect();
            if permanent {
                assert_eq!(*delete.calls.lock().unwrap(), expected_paths);
                assert!(report
                    .results
                    .iter()
                    .all(|item| item.outcome == ItemOutcome::Deleted));
                assert!(trash.moved().is_empty());
            } else {
                assert_eq!(trash.moved(), expected_paths);
                assert!(report
                    .results
                    .iter()
                    .all(|item| item.outcome == ItemOutcome::MovedToTrash));
                assert!(delete.calls.lock().unwrap().is_empty());
            }
            assert!(!report
                .results
                .iter()
                .any(|item| [GROUP, ODD].contains(&item.index)));
        }
    }
}

#[test]
fn permanent_failures_preserve_outcomes_and_never_fall_back_to_trash() {
    for failed_index in [BUNDLE, CACHE] {
        let world = World::new();
        let lease = world.store.lease().unwrap();
        let trash = TestTrash::new();
        let delete = DeleteSink {
            calls: Mutex::default(),
            fail_on: Some(world.path(failed_index)),
        };
        let ctx = UninstallContext {
            home: &world.home,
            probe: &MockChecker::none(),
            trash: &trash,
            cancel: None,
            progress: None,
            policy: &lease,
        };
        let report = crate::uninstall::execute_prepared_with(
            &world.plan,
            Consent::Explicit,
            &ctx,
            Some(&delete),
        )
        .unwrap();
        assert!(
            matches!(&report.results[failed_index].outcome, ItemOutcome::DeleteFailed { detail, source_present: true } if detail.contains("fixture deletion refused"))
        );
        assert!(trash.moved().is_empty());
        if failed_index == BUNDLE {
            assert!(report.results[1..]
                .iter()
                .all(|item| item.outcome == ItemOutcome::NotAttempted));
            assert_eq!(*delete.calls.lock().unwrap(), [world.path(BUNDLE)]);
        } else {
            assert_eq!(report.results[0].outcome, ItemOutcome::Deleted);
            assert_eq!(report.results[2].outcome, ItemOutcome::NotAttempted);
            assert_eq!(
                *delete.calls.lock().unwrap(),
                [world.path(BUNDLE), world.path(CACHE)]
            );
        }
    }
}

#[test]
fn cli_stops_after_cache_failure_while_default_checked_execution_continues() {
    struct SelectiveTrash {
        calls: Mutex<Vec<PathBuf>>,
        failed: PathBuf,
    }
    impl Trash for SelectiveTrash {
        fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
            self.calls.lock().unwrap().push(path.to_path_buf());
            if path == self.failed {
                Err(TrashError::Failed("fixture move refused".into()))
            } else {
                Ok(())
            }
        }
    }
    for cli in [true, false] {
        for permanent in [true, false] {
            let world = World::new();
            let lease = world.store.lease().unwrap();
            let trash = SelectiveTrash {
                calls: Mutex::default(),
                failed: world.path(CACHE),
            };
            let delete = DeleteSink {
                calls: Mutex::default(),
                fail_on: Some(world.path(CACHE)),
            };
            let ctx = UninstallContext {
                home: &world.home,
                probe: &MockChecker::none(),
                trash: &trash,
                cancel: None,
                progress: None,
                policy: &lease,
            };
            let report = if cli {
                crate::uninstall::execute_prepared_with(
                    &world.plan,
                    Consent::Explicit,
                    &ctx,
                    permanent.then_some(&delete as &dyn Delete),
                )
                .unwrap()
            } else if permanent {
                execute_checked_uninstall_action(
                    &world.plan,
                    &BTreeSet::from([BUNDLE, CACHE, DATA]),
                    &ctx,
                    Some(&delete),
                )
                .unwrap()
            } else {
                execute_checked_uninstall(&world.plan, &BTreeSet::from([BUNDLE, CACHE, DATA]), &ctx)
                    .unwrap()
            };
            assert!(matches!(
                report.results[1].outcome,
                ItemOutcome::DeleteFailed { .. } | ItemOutcome::Failed { .. }
            ));
            let expected = if cli {
                vec![world.path(BUNDLE), world.path(CACHE)]
            } else {
                vec![world.path(BUNDLE), world.path(CACHE), world.path(DATA)]
            };
            if permanent {
                assert_eq!(*delete.calls.lock().unwrap(), expected);
                assert!(
                    trash.calls.lock().unwrap().is_empty(),
                    "never fall back to Trash"
                );
            } else {
                assert_eq!(*trash.calls.lock().unwrap(), expected);
                assert!(delete.calls.lock().unwrap().is_empty());
            }
            assert_eq!(
                report.results[2].outcome,
                if cli {
                    ItemOutcome::NotAttempted
                } else if permanent {
                    ItemOutcome::Deleted
                } else {
                    ItemOutcome::MovedToTrash
                }
            );
        }
    }
}

#[test]
fn cli_stops_after_a_kept_cache_refusal() {
    let world = World::new();
    world.keep(&world.path(CACHE), RuleScope::Directory);
    let lease = world.store.lease().unwrap();
    let trash = TestTrash::new();
    let ctx = UninstallContext {
        home: &world.home,
        probe: &MockChecker::none(),
        trash: &trash,
        cancel: None,
        progress: None,
        policy: &lease,
    };
    let report =
        crate::uninstall::execute_prepared_with(&world.plan, Consent::Explicit, &ctx, None)
            .unwrap();
    assert_eq!(report.results[0].outcome, ItemOutcome::MovedToTrash);
    assert!(matches!(
        report.results[1].outcome,
        ItemOutcome::Skipped(SkipReason::Kept { .. })
    ));
    assert_eq!(report.results[2].outcome, ItemOutcome::NotAttempted);
    assert_eq!(trash.moved(), [world.path(BUNDLE)]);
}
