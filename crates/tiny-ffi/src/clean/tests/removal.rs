//! The delete mode and the Trash row over FFI: what a preview decides per
//! item, what `clean_remove` does and reports, and that old callers keep the
//! Trash-only flow. The fake routes change nothing on disk unless a test
//! uses `SafeDelete` on its own temp dir.

use std::fs;

use tiny_core::clean::fs_safe::SafeDelete;

use super::*;

/// `logs/a` (Rebuilt automatically), `targets/t` (Project build output),
/// `downloads/d` (Your files) and `.Trash` holding `old` (the Trash row).
fn layout(label: &str) -> PathBuf {
    let dir = fixture_dir(label, &[], 0);
    for (folder, name) in [
        ("logs", "a"),
        ("targets", "t"),
        ("downloads", "d"),
        (".Trash", "old"),
    ] {
        fs::create_dir(dir.join(folder)).unwrap();
        fs::write(dir.join(folder).join(name), b"1234").unwrap();
    }
    dir
}

/// Registered ids, so each category sits in its real section.
fn providers(dir: &Path) -> Vec<Box<dyn CleanProvider>> {
    let trash = Fixture {
        risk: RiskLevel::Destructive,
        desktop: false,
        empties_trash: true,
        extra: vec![dir.join(".Trash")],
        ..Fixture::new("trash", &dir.join("no-such-folder"))
    };
    vec![
        Box::new(Fixture::new("user-logs", &dir.join("logs"))),
        Box::new(Fixture::new("rust-targets", &dir.join("targets"))),
        Box::new(Fixture::new("downloads-old", &dir.join("downloads"))),
        Box::new(trash),
    ]
}

fn with_trash() -> CleanOptions {
    CleanOptions {
        include_trash: true,
        ..CleanOptions::default()
    }
}

/// Candidates of the categories the app may act on.
fn movable(discovery: &FfiDiscovery) -> Vec<String> {
    discovery
        .categories
        .iter()
        .filter(|c| !matches!(c.desktop_action, FfiDesktopAction::ReportOnly { .. }))
        .flat_map(|c| c.candidates.iter().map(|c| c.id.clone()))
        .collect()
}

fn permanent(preview: &FfiPreview) -> Vec<(&str, bool)> {
    preview
        .items
        .iter()
        .map(|i| (i.category_id.as_str(), i.permanent))
        .collect()
}

fn run_remove(
    session: &TinySession,
    preview_id: &str,
    dir: &Path,
    trash: &FakeTrash,
    delete: &dyn Delete,
) -> Result<FfiRemovalReport, FfiError> {
    let dir = dir.to_path_buf();
    let providers = move |_: &CleanOptions| providers(&dir);
    session.remove_clean(
        preview_id,
        &providers,
        &NotRunning,
        trash,
        delete,
        trash,
        &CancellationToken::default(),
        &Progress::default(),
    )
}

/// The Trash-only `clean_execute` on the same providers.
fn run_execute(
    session: &TinySession,
    preview_id: &str,
    dir: &Path,
    trash: &FakeTrash,
) -> Result<FfiExecReport, FfiError> {
    let dir = dir.to_path_buf();
    let providers = move |_: &CleanOptions| providers(&dir);
    session.execute_clean(
        preview_id,
        &providers,
        &NotRunning,
        trash,
        &CancellationToken::default(),
        &Progress::default(),
    )
}

fn removal_outcomes(report: &FfiRemovalReport) -> Vec<FfiRemovalOutcome> {
    report.results.iter().map(|r| r.outcome.clone()).collect()
}

#[test]
fn old_callers_keep_the_trash_only_flow() {
    let dir = layout("old-callers");
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let trash_row = discovery.categories.iter().find(|c| c.id == "trash");
    assert_eq!(
        trash_row.unwrap().desktop_action,
        FfiDesktopAction::ReportOnly {
            reason: FfiReportOnlyReason::Destructive
        },
        "no Trash row unless the app asks for it"
    );
    let preview = session.clean_preview(movable(&discovery)).unwrap();
    assert!(preview.items.iter().all(|i| !i.permanent));
    assert_eq!(preview.bytes_to_free, 0);
    assert_eq!(preview.bytes_to_trash, preview.bytes_selected);

    let trash = FakeTrash::default();
    let report = run_execute(&session, &preview.preview_id, &dir, &trash).unwrap();
    assert_eq!(outcomes(&report), vec![FfiItemOutcome::MovedToTrash; 3]);
    assert_eq!(trash.steps(), ["trash d", "trash a", "trash t"]);
    let options = |include_trash| FfiCleanOptions {
        categories: Vec::new(),
        include_review: true,
        include_destructive: false,
        idle_days: 30,
        all_build_output: true,
        include_trash,
        tool_commands: false,
    };
    assert!(CleanOptions::from(options(true)).include_trash);
    assert!(!CleanOptions::from(options(false)).include_trash);
    remove(&dir);
}

#[test]
fn permanent_mode_deletes_what_comes_back_and_trashes_your_files() {
    let dir = layout("permanent");
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let preview = session
        .clean_preview_with_mode(movable(&discovery), FfiDeleteMode::Permanent)
        .unwrap();
    assert_eq!(
        permanent(&preview),
        [
            ("downloads-old", false),
            ("user-logs", true),
            ("rust-targets", true)
        ]
    );
    let size = |path: &str| allocated(&dir.join(path));
    assert_eq!(preview.bytes_to_free, size("logs/a") + size("targets/t"));
    assert_eq!(preview.bytes_to_trash, size("downloads/d"));

    let sinks = FakeTrash::default();
    let report = run_remove(&session, &preview.preview_id, &dir, &sinks, &sinks).unwrap();
    assert_eq!(
        removal_outcomes(&report),
        [
            FfiRemovalOutcome::MovedToTrash,
            FfiRemovalOutcome::Deleted,
            FfiRemovalOutcome::Deleted
        ]
    );
    assert_eq!(sinks.steps(), ["delete a", "delete t", "trash d"]);
    assert_eq!(report.bytes_freed, preview.bytes_to_free);
    assert_eq!(report.bytes_moved_to_trash, preview.bytes_to_trash);
    assert_eq!((report.deleted_count, report.moved_count), (2, 1));
    remove(&dir);
}

#[test]
fn trash_mode_moves_everything_to_the_trash() {
    let dir = layout("trash-mode");
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let preview = session
        .clean_preview_with_mode(movable(&discovery), FfiDeleteMode::Trash)
        .unwrap();
    assert!(preview.items.iter().all(|i| !i.permanent));
    let sinks = FakeTrash::default();
    let report = run_remove(&session, &preview.preview_id, &dir, &sinks, &sinks).unwrap();
    assert_eq!(
        removal_outcomes(&report),
        vec![FfiRemovalOutcome::MovedToTrash; 3]
    );
    assert_eq!(sinks.steps(), ["trash d", "trash a", "trash t"]);
    assert_eq!(report.bytes_freed, 0, "moved, not freed");
    remove(&dir);
}

#[test]
fn the_old_execute_refuses_a_permanent_preview_without_spending_it() {
    let dir = layout("old-execute");
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let preview = session
        .clean_preview_with_mode(movable(&discovery), FfiDeleteMode::Permanent)
        .unwrap();
    let trash = FakeTrash::default();
    let refused = run_execute(&session, &preview.preview_id, &dir, &trash);
    assert!(matches!(refused, Err(FfiError::InvalidInput { .. })));
    assert!(trash.steps().is_empty());
    let report = run_remove(&session, &preview.preview_id, &dir, &trash, &trash).unwrap();
    assert_eq!(report.deleted_count, 2, "the preview was still unspent");
    remove(&dir);
}

#[test]
fn permanent_deletion_removes_only_the_previewed_path() {
    let dir = layout("real-delete");
    fs::write(dir.join("logs/sibling"), b"stays").unwrap();
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let a = id_of(&discovery, "logs/a");
    let preview = session
        .clean_preview_with_mode(vec![a], FfiDeleteMode::Permanent)
        .unwrap();
    let trash = FakeTrash::default();
    let report = run_remove(&session, &preview.preview_id, &dir, &trash, &SafeDelete).unwrap();
    assert_eq!(removal_outcomes(&report), [FfiRemovalOutcome::Deleted]);
    assert!(fs::symlink_metadata(dir.join("logs/a")).is_err());
    assert!(dir.join("logs/sibling").exists() && dir.join("targets/t").exists());
    assert!(trash.steps().is_empty());
    remove(&dir);
}

#[test]
fn a_review_item_needs_explicit_consent_in_both_modes() {
    let dir = review_fixture("review-modes");
    let mut jetbrains = Fixture::new("jetbrains", &dir.join("Logs/JetBrains"));
    jetbrains.risk = RiskLevel::Review;
    let providers: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(Fixture::new("user-logs", &dir.join("Logs"))),
        Box::new(jetbrains),
    ];
    let session = TestSession::new();
    for mode in [FfiDeleteMode::Trash, FfiDeleteMode::Permanent] {
        let discovery = discover(&session, &providers).unwrap();
        let logs = candidate_ids(&discovery, "user-logs");
        let preview = session.clean_preview_with_mode(logs, mode).unwrap();
        let blocked = preview
            .excluded
            .iter()
            .find(|e| e.path.ends_with("Logs/JetBrains"));
        assert!(
            blocked.is_some_and(|e| matches!(
                e.reason,
                FfiExclusionReason::CoversUnselectedReview { .. }
            )),
            "{mode:?}"
        );
        assert!(preview
            .items
            .iter()
            .all(|i| !i.path.ends_with("Logs/JetBrains")));
    }
    remove(&dir);
}

#[test]
fn report_only_items_never_reach_a_preview_in_either_mode() {
    let dir = layout("report-only");
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let row = candidate_ids(&discovery, "trash");
    for mode in [FfiDeleteMode::Trash, FfiDeleteMode::Permanent] {
        let refused = session.clean_preview_with_mode(row.clone(), mode);
        assert!(matches!(refused, Err(FfiError::InvalidInput { .. })));
    }
    remove(&dir);
}

#[test]
fn the_trash_row_is_emptied_only_when_selected_and_before_any_move() {
    let dir = layout("trash-row");
    let session = TestSession::new();
    let discovery = discover_with(&session, &providers(&dir), with_trash()).unwrap();
    let row = discovery
        .categories
        .iter()
        .find(|c| c.id == "trash")
        .unwrap();
    assert_eq!(row.desktop_action, FfiDesktopAction::EmptyTrash);
    assert_eq!(row.section, Some(FfiCleanSection::YourFiles));
    assert_eq!(
        row.candidates[0].risk,
        FfiRisk::Destructive,
        "never preselected"
    );
    let row_id = row.candidates[0].id.clone();
    let others: Vec<String> = movable(&discovery)
        .into_iter()
        .filter(|id| *id != row_id)
        .collect();

    let preview = session
        .clean_preview_with_mode(others, FfiDeleteMode::Permanent)
        .unwrap();
    let sinks = FakeTrash::default();
    run_remove(&session, &preview.preview_id, &dir, &sinks, &sinks).unwrap();
    assert!(
        !sinks.steps().contains(&"empty".to_string()),
        "not selected"
    );

    let discovery = discover_with(&session, &providers(&dir), with_trash()).unwrap();
    let preview = session
        .clean_preview_with_mode(movable(&discovery), FfiDeleteMode::Trash)
        .unwrap();
    assert_eq!(
        permanent(&preview)[0],
        ("trash", true),
        "permanent by nature"
    );
    let sinks = FakeTrash::default();
    let report = run_remove(&session, &preview.preview_id, &dir, &sinks, &sinks).unwrap();
    assert_eq!(report.results[0].outcome, FfiRemovalOutcome::TrashEmptied);
    assert_eq!(
        sinks.steps(),
        ["empty", "trash d", "trash a", "trash t"],
        "emptied before this run moved anything there"
    );
    assert_eq!(report.bytes_freed, allocated(&dir.join(".Trash/old")));
    assert_eq!(report.deleted_count, 1);
    remove(&dir);
}

#[test]
fn a_failure_is_never_retried_another_way() {
    let dir = layout("no-fallback");
    let session = TestSession::new();
    let discovery = discover(&session, &providers(&dir)).unwrap();
    let preview = session
        .clean_preview_with_mode(movable(&discovery), FfiDeleteMode::Permanent)
        .unwrap();
    let sinks = FakeTrash {
        fail_on: Some("d"),
        fail_delete_on: Some("a"),
        ..Default::default()
    };
    let report = run_remove(&session, &preview.preview_id, &dir, &sinks, &sinks).unwrap();
    let failure = |outcome: &FfiRemovalOutcome| match outcome {
        FfiRemovalOutcome::Failed { reason, .. } => Some(*reason),
        _ => None,
    };
    let reasons: Vec<_> = report.results.iter().map(|r| failure(&r.outcome)).collect();
    assert_eq!(
        reasons,
        [
            Some(FfiRemovalFailure::TrashFailed),
            Some(FfiRemovalFailure::DeleteFailed),
            None
        ]
    );
    assert_eq!(
        sinks.steps(),
        ["delete t"],
        "a failed move is not deleted, a failed deletion is not moved or escalated"
    );
    assert_eq!((report.failed_count, report.deleted_count), (2, 1));
    assert_eq!(report.stopped, None);
    remove(&dir);
}

#[test]
fn the_total_counts_a_hard_link_split_between_selected_items_once() {
    let dir = fixture_dir("split-link", &[], 0);
    for project in ["one", "two"] {
        fs::create_dir_all(dir.join("targets").join(project)).unwrap();
        fs::write(dir.join("targets").join(project).join("own"), [1u8; 9000]).unwrap();
    }
    fs::write(dir.join("targets/one/linked"), [2u8; 50_000]).unwrap();
    fs::hard_link(
        dir.join("targets/one/linked"),
        dir.join("targets/two/linked"),
    )
    .unwrap();
    let providers: Vec<Box<dyn CleanProvider>> =
        vec![Box::new(Fixture::new("rust-targets", &dir.join("targets")))];
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    let own = allocated(&dir.join("targets/one/own"));
    let linked = allocated(&dir.join("targets/one/linked"));
    let sizes: Vec<u64> = discovery.categories[0]
        .candidates
        .iter()
        .map(|c| c.size_bytes)
        .collect();
    assert_eq!(sizes, [own, own], "alone, neither frees the linked file");

    let one = session
        .clean_preview_with_mode(vec![id_of(&discovery, "one")], FfiDeleteMode::Permanent)
        .unwrap();
    assert_eq!(one.bytes_to_free, own);
    let both = session
        .clean_preview_with_mode(ids(&discovery), FfiDeleteMode::Permanent)
        .unwrap();
    assert_eq!(both.bytes_to_free, 2 * own + linked, "counted once");
    assert_eq!(both.bytes_selected, 2 * own, "the plain sum is unchanged");
    // Only macOS can tell which blocks other files share.
    assert_eq!(
        both.bytes_to_free_is_upper_bound,
        cfg!(not(target_os = "macos"))
    );
    remove(&dir);
}
