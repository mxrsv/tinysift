//! Adapter tests. They use only temp dirs they create, injected probes and
//! a fake `Trash`: never Finder, `osascript` or real user data. Shared
//! fixtures live in `test_support`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use tiny_core::clean::types::RiskLevel;

use super::test_support::*;
use super::*;

mod build_output;
mod execute_protection;
mod protection;
mod protection_writes;
mod selection;

// ---------- discovery and preview (T4b) ----------

#[test]
fn preview_is_built_from_discovered_ids_only() {
    let dir = fixture_dir("preview", &["a", "b"], 4);
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("logs", &dir))];
    let session = TestSession::new();
    let progress = Progress::default();
    let discovery = session
        .discover_clean(
            CleanOptions::default(),
            &providers,
            &NotRunning,
            &CancellationToken::default(),
            &progress,
        )
        .unwrap();
    assert!(
        !progress.0.lock().unwrap().is_empty(),
        "progress reached the caller"
    );
    let category = &discovery.categories[0];
    assert_eq!(category.status, FfiCategoryStatus::Found);
    assert_eq!(category.desktop_action, FfiDesktopAction::MoveToTrash);
    let freed = allocated(&dir.join("a")) + allocated(&dir.join("b"));
    assert_eq!(category.total_bytes, freed, "freed bytes, not length");
    // Only macOS can tell which blocks other files share.
    assert!(category
        .candidates
        .iter()
        .all(|c| c.size_is_upper_bound == cfg!(not(target_os = "macos"))));
    assert_eq!(category.family, None, "fixture IDs are not registered");
    assert_eq!(category.inclusion_reason, "fixture");

    let preview = session.clean_preview(ids(&discovery)).unwrap();
    assert_eq!(preview.items.len(), 2);
    assert_eq!(preview.bytes_selected, freed);
    assert!(preview.excluded.is_empty());
    assert_eq!(preview.expires_in_seconds, PREVIEW_TTL.as_secs());

    let forged = session.clean_preview(vec!["d1-99".into()]);
    assert!(matches!(forged, Err(FfiError::PreviewInvalid { .. })));
    let empty = session.clean_preview(Vec::new());
    assert!(matches!(empty, Err(FfiError::InvalidInput { .. })));
    remove(&dir);
}

#[test]
fn a_new_discovery_invalidates_old_candidate_ids() {
    let dir = fixture_dir("replaced", &["a"], 1);
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("logs", &dir))];
    let session = TestSession::new();
    let old = ids(&discover(&session, &providers).unwrap());
    let new = ids(&discover(&session, &providers).unwrap());
    assert_ne!(old, new);
    assert!(matches!(
        session.clean_preview(old),
        Err(FfiError::PreviewInvalid { .. })
    ));
    assert!(session.clean_preview(new).is_ok());
    remove(&dir);
}

#[test]
fn overlapping_selections_are_excluded_with_a_reason() {
    let dir = fixture_dir("overlap", &[], 0);
    std::fs::create_dir(dir.join("Caches")).unwrap();
    std::fs::write(dir.join("Caches/x"), b"12").unwrap();
    // `caches` lists `Caches`; `browser` finds `Caches/x` and `Caches`.
    let mut browser = Fixture::new("browser", &dir.join("Caches"));
    browser.extra = vec![dir.join("Caches")];
    let providers: Vec<Box<dyn CleanProvider>> =
        vec![Box::new(Fixture::new("caches", &dir)), Box::new(browser)];
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    let preview = session.clean_preview(ids(&discovery)).unwrap();
    assert_eq!(preview.items.len(), 1);
    assert_eq!(
        preview.bytes_selected,
        allocated(&dir.join("Caches/x")),
        "covered bytes are not double-counted"
    );
    let kept = preview.items[0].candidate_id.clone();
    let reasons: Vec<_> = preview.excluded.iter().map(|e| e.reason.clone()).collect();
    assert!(reasons.contains(&FfiExclusionReason::Duplicate {
        kept_candidate_id: kept.clone()
    }));
    assert!(reasons.contains(&FfiExclusionReason::InsideSelected {
        parent_candidate_id: kept
    }));
    remove(&dir);
}

#[test]
fn report_only_categories_cannot_be_previewed() {
    let dir = fixture_dir("report-only", &[], 0);
    // Like docker: a tool placeholder in a provider that is not opted in.
    let mut docker = Fixture::new("docker", &dir);
    docker.desktop = false;
    docker.extra = vec![PathBuf::from("<docker:images>")];
    let mut destructive = Fixture::new("trash", &dir);
    destructive.risk = RiskLevel::Destructive;
    destructive.extra = vec![dir.clone()];
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(docker), Box::new(destructive)];
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    let actions: Vec<_> = discovery
        .categories
        .iter()
        .map(|c| c.desktop_action)
        .collect();
    assert_eq!(
        actions,
        vec![
            FfiDesktopAction::ReportOnly {
                reason: FfiReportOnlyReason::NotPerPathTrash
            },
            FfiDesktopAction::ReportOnly {
                reason: FfiReportOnlyReason::Destructive
            },
        ]
    );
    for category in &discovery.categories {
        let id = category.candidates[0].id.clone();
        assert!(matches!(
            session.clean_preview(vec![id]),
            Err(FfiError::InvalidInput { .. })
        ));
    }
    remove(&dir);
}

#[test]
fn a_category_root_at_home_is_refused_at_discovery() {
    let home = fixture_dir("home", &["a"], 1);
    let mut tool_like = Fixture::new("npm", &home.join("none"));
    tool_like.extra = vec![home.clone()];
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(tool_like)];
    let session = TestSession::new();
    session.clean_state().home_inputs = Some((Some(home.clone()), Some(home.clone())));
    let discovery = discover(&session, &providers).unwrap();
    let category = &discovery.categories[0];
    assert!(category.candidates.is_empty());
    assert_eq!(
        category.refused,
        vec![FfiRefusedPath {
            path: home.to_string_lossy().into_owned(),
            reason: "the home folder".into()
        }]
    );
    remove(&home);
}

#[test]
fn an_untrusted_home_fails_before_any_scan_or_move() {
    let dir = fixture_dir("untrusted-home", &["a"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let account = Some(dir.clone());
    for env in [dir.join("other"), PathBuf::from("/")] {
        session.clean_state().home_inputs = Some((Some(env.clone()), account.clone()));
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("logs", &dir))];
        let scan = discover(&session, &providers);
        assert!(
            matches!(scan, Err(FfiError::UntrustedHome { .. })),
            "{env:?}"
        );
        let trash = FakeTrash::default();
        let run = execute(
            &session,
            &preview.preview_id,
            &dir,
            &trash,
            &CancellationToken::default(),
        );
        assert!(
            matches!(run, Err(FfiError::UntrustedHome { .. })),
            "{env:?}"
        );
        assert!(trash.moved.lock().unwrap().is_empty());
        assert!(!session.is_busy());
    }
    // Nothing was scanned or consumed: the preview still runs once HOME is trusted.
    session.clean_state().home_inputs = Some((Some(dir.clone()), account));
    let report = execute(
        &session,
        &preview.preview_id,
        &dir,
        &FakeTrash::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(report.moved_count, 1);
    remove(&dir);
}

#[test]
fn cancelled_discovery_stores_nothing() {
    let dir = fixture_dir("cancel", &["a"], 1);
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("logs", &dir))];
    let session = TestSession::new();
    let token = CancellationToken::default();
    token.cancel();
    let result = session.discover_clean(
        CleanOptions::default(),
        &providers,
        &NotRunning,
        &token,
        &Progress::default(),
    );
    assert!(matches!(result, Err(FfiError::Cancelled)));
    assert!(!session.is_busy());
    assert!(matches!(
        session.clean_preview(vec!["d1-0".into()]),
        Err(FfiError::PreviewInvalid { .. })
    ));
    remove(&dir);
}

#[test]
fn discovery_is_rejected_while_another_operation_runs() {
    let session = TestSession::new();
    let _gate = session.begin().unwrap();
    assert!(matches!(discover(&session, &[]), Err(FfiError::Busy)));
}

#[test]
fn invalid_options_are_rejected_before_scanning() {
    let session = TestSession::new();
    let bad = CleanOptions {
        idle_days: 0,
        ..CleanOptions::default()
    };
    let result = session.discover_clean(
        bad,
        &[],
        &NotRunning,
        &CancellationToken::default(),
        &Progress::default(),
    );
    assert!(matches!(result, Err(FfiError::InvalidInput { .. })));
}

#[test]
fn failing_and_app_gated_categories_are_reported_beside_healthy_ones() {
    let dir = fixture_dir("isolate", &["a"], 1);
    let mut gated = Fixture::new("gated", &dir);
    gated.app = Some("Xcode");
    let providers: Vec<Box<dyn CleanProvider>> =
        vec![Box::new(gated), Box::new(Fixture::new("logs", &dir))];
    let session = TestSession::new();
    struct XcodeRunning;
    impl AppProbe for XcodeRunning {
        fn probe(&self, name: &str) -> std::result::Result<bool, String> {
            Ok(name == "Xcode")
        }
    }
    let discovery = session
        .discover_clean(
            CleanOptions::default(),
            &providers,
            &XcodeRunning,
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap();
    assert_eq!(
        discovery.categories[0].status,
        FfiCategoryStatus::AppRunning {
            app: "Xcode".into()
        }
    );
    assert_eq!(discovery.categories[1].status, FfiCategoryStatus::Found);
    remove(&dir);
}

#[test]
fn execute_moves_previewed_paths_and_consumes_the_preview() {
    let dir = fixture_dir("exec", &["a", "b"], 4);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let trash = FakeTrash::default();
    let token = CancellationToken::default();
    let report = execute(&session, &preview.preview_id, &dir, &trash, &token).unwrap();
    assert_eq!(outcomes(&report), vec![FfiItemOutcome::MovedToTrash; 2]);
    let freed = allocated(&dir.join("a")) + allocated(&dir.join("b"));
    assert_eq!(report.bytes_selected, freed);
    assert_eq!(report.bytes_moved_to_trash, freed);
    assert_eq!(report.moved_count, 2);
    assert_eq!(report.stopped, None);
    assert_eq!(trash.moved.lock().unwrap().len(), 2);

    let replay = execute(&session, &preview.preview_id, &dir, &trash, &token);
    assert!(matches!(replay, Err(FfiError::PreviewInvalid { detail }) if detail.contains("used")));
    assert_eq!(trash.moved.lock().unwrap().len(), 2, "no second mutation");
    remove(&dir);
}

#[test]
fn forged_expired_and_replaced_previews_fail_without_mutation() {
    let dir = fixture_dir("invalid", &["a"], 1);
    let session = TestSession::new();
    let trash = FakeTrash::default();
    let token = CancellationToken::default();

    let forged = execute(&session, "p999", &dir, &trash, &token);
    assert!(matches!(forged, Err(FfiError::PreviewInvalid { .. })));

    session.clean_state().ttl = Some(Duration::ZERO);
    let expired = preview_all(&session, &dir);
    let result = execute(&session, &expired.preview_id, &dir, &trash, &token);
    assert!(
        matches!(result, Err(FfiError::PreviewInvalid { detail }) if detail.contains("expired"))
    );
    session.clean_state().ttl = None;

    let first = preview_all(&session, &dir);
    let _second = preview_all(&session, &dir);
    let replaced = execute(&session, &first.preview_id, &dir, &trash, &token);
    assert!(matches!(replaced, Err(FfiError::PreviewInvalid { .. })));

    assert!(trash.moved.lock().unwrap().is_empty());
    assert!(dir.join("a").exists());
    remove(&dir);
}

#[test]
fn a_preview_expires_by_wall_clock_even_if_the_mac_slept() {
    let dir = fixture_dir("sleep", &["a"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    // Sleep stops `Instant`; only the wall-clock deadline has passed.
    session
        .clean_state()
        .preview
        .as_mut()
        .unwrap()
        .expires_at_wall = SystemTime::now() - Duration::from_secs(1);
    let trash = FakeTrash::default();
    let result = execute(
        &session,
        &preview.preview_id,
        &dir,
        &trash,
        &CancellationToken::default(),
    );
    assert!(
        matches!(result, Err(FfiError::PreviewInvalid { detail }) if detail.contains("expired"))
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    remove(&dir);
}

#[test]
fn partial_results_report_each_item() {
    let dir = fixture_dir("partial", &["edited", "fails", "ok", "swapped"], 4);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    // Four equal files, measured at discovery.
    let one = allocated(&dir.join("ok"));
    std::fs::write(dir.join("edited"), b"changed after preview").unwrap();
    std::fs::rename(dir.join("swapped"), dir.join("real")).unwrap();
    std::os::unix::fs::symlink(dir.join("real"), dir.join("swapped")).unwrap();
    let trash = FakeTrash {
        fail_on: Some("fails"),
        ..Default::default()
    };
    let report = execute(
        &session,
        &preview.preview_id,
        &dir,
        &trash,
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(
        outcomes(&report),
        vec![
            FfiItemOutcome::Skipped {
                reason: FfiSkipReason::Changed,
                detail: None
            },
            FfiItemOutcome::Failed {
                reason: FfiTrashFailure::TrashFailed,
                detail: "Finder error".into(),
                source_still_present: true,
            },
            FfiItemOutcome::MovedToTrash,
            FfiItemOutcome::Skipped {
                reason: FfiSkipReason::Symlink,
                detail: None
            },
        ]
    );
    assert_eq!(
        (
            report.moved_count,
            report.failed_count,
            report.skipped_count
        ),
        (1, 1, 2)
    );
    assert_eq!(report.bytes_selected, 4 * one);
    assert_eq!(report.bytes_moved_to_trash, one);
    assert!(
        dir.join("fails").exists(),
        "a Trash failure keeps the source"
    );
    remove(&dir);
}

#[test]
fn cancellation_before_execute_moves_nothing_and_reports_not_attempted() {
    let dir = fixture_dir("exec-cancel", &["a", "b"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let token = CancellationToken::default();
    token.cancel();
    let trash = FakeTrash::default();
    let report = execute(&session, &preview.preview_id, &dir, &trash, &token).unwrap();
    assert_eq!(report.stopped, Some(FfiStopReason::Cancelled));
    assert_eq!(outcomes(&report), vec![FfiItemOutcome::NotAttempted; 2]);
    assert_eq!(report.bytes_moved_to_trash, 0);
    assert!(trash.moved.lock().unwrap().is_empty());
    remove(&dir);
}

#[test]
fn automation_denial_stops_with_a_report_and_consumes_the_preview() {
    let dir = fixture_dir("denied", &["a", "b"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let trash = FakeTrash {
        deny: true,
        ..Default::default()
    };
    let token = CancellationToken::default();
    let report = execute(&session, &preview.preview_id, &dir, &trash, &token).unwrap();
    assert_eq!(report.stopped, Some(FfiStopReason::AutomationDenied));
    assert_eq!(
        outcomes(&report),
        vec![
            FfiItemOutcome::Failed {
                reason: FfiTrashFailure::AutomationDenied,
                detail: "(-1743)".into(),
                source_still_present: true,
            },
            FfiItemOutcome::NotAttempted,
        ]
    );
    assert_eq!(report.moved_count, 0);
    assert!(dir.join("a").exists() && dir.join("b").exists());
    let retry = execute(
        &session,
        &preview.preview_id,
        &dir,
        &FakeTrash::default(),
        &token,
    );
    assert!(
        matches!(retry, Err(FfiError::PreviewInvalid { .. })),
        "a denied run still consumed the preview; the UI must rescan"
    );
    remove(&dir);
}

#[test]
fn panic_mid_execute_keeps_the_preview_consumed_and_releases_the_gate() {
    let dir = fixture_dir("panic", &["a", "b"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let trash = FakeTrash {
        panic_on: Some("b"),
        ..Default::default()
    };
    let token = CancellationToken::default();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        execute(&session, &preview.preview_id, &dir, &trash, &token)
    }));
    assert!(outcome.is_err(), "the panic surfaces as a failed call");
    assert!(!session.is_busy(), "the gate is released");
    let replay = execute(&session, &preview.preview_id, &dir, &trash, &token);
    assert!(matches!(replay, Err(FfiError::PreviewInvalid { .. })));
    assert!(
        session.clean_preview(vec!["d1-0".into()]).is_ok(),
        "state lock still usable"
    );
    remove(&dir);
}

#[test]
fn execute_while_busy_is_rejected_without_consuming_the_preview() {
    let dir = fixture_dir("busy", &["a"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let trash = FakeTrash::default();
    let token = CancellationToken::default();
    {
        let _gate = session.begin().unwrap();
        let busy = execute(&session, &preview.preview_id, &dir, &trash, &token);
        assert!(matches!(busy, Err(FfiError::Busy)));
    }
    let report = execute(&session, &preview.preview_id, &dir, &trash, &token).unwrap();
    assert_eq!(report.moved_count, 1);
    remove(&dir);
}

#[test]
fn execution_revalidates_running_apps_and_failed_probes() {
    let dir = fixture_dir("apps", &["a"], 1);
    let session = TestSession::new();
    let preview = preview_all(&session, &dir);
    let root = dir.clone();
    let providers = move |_: &CleanOptions| -> Vec<Box<dyn CleanProvider>> {
        let mut gated = Fixture::new("logs", &root);
        gated.app = Some("Xcode");
        vec![Box::new(gated)]
    };
    struct ProbeFails;
    impl AppProbe for ProbeFails {
        fn probe(&self, _: &str) -> std::result::Result<bool, String> {
            Err("pgrep timed out".into())
        }
    }
    let trash = FakeTrash::default();
    let report = session
        .execute_clean(
            &preview.preview_id,
            &providers,
            &ProbeFails,
            &trash,
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap();
    assert!(matches!(
        &outcomes(&report)[0],
        FfiItemOutcome::Skipped { reason: FfiSkipReason::SafetyCheckUnavailable, detail: Some(d) } if d.contains("Xcode")
    ));
    assert!(trash.moved.lock().unwrap().is_empty());
    remove(&dir);
}
