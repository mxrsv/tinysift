//! A scan replaces only the categories it covers. These tests scan the
//! categories of one session at different times and check what stays valid,
//! what is refused, and what is recomputed over the merged set.

use std::fs;
use std::sync::Mutex;

use super::*;

/// Scans only `categories`, with the given providers.
fn scan(
    session: &TinySession,
    categories: &[&str],
    providers: &[Box<dyn CleanProvider>],
) -> Result<FfiDiscovery, FfiError> {
    discover_with(session, providers, covering(categories))
}

fn covering(categories: &[&str]) -> CleanOptions {
    CleanOptions {
        category: categories.iter().map(|c| c.to_string()).collect(),
        ..CleanOptions::default()
    }
}

fn one(id: &'static str, root: &Path) -> Vec<Box<dyn CleanProvider>> {
    vec![Box::new(Fixture::new(id, root))]
}

fn category_ids(discovery: &FfiDiscovery) -> Vec<&str> {
    discovery.categories.iter().map(|c| c.id.as_str()).collect()
}

fn invalid<T: std::fmt::Debug>(result: Result<T, FfiError>) -> bool {
    matches!(result, Err(FfiError::PreviewInvalid { .. }))
}

#[test]
fn a_rescan_invalidates_only_its_categories_ids() {
    let dir = fixture_dir("rescan-one", &[], 0);
    for name in ["a", "b"] {
        fs::create_dir(dir.join(name)).unwrap();
        fs::write(dir.join(name).join("file"), b"1234").unwrap();
    }
    let both: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(Fixture::new("a", &dir.join("a"))),
        Box::new(Fixture::new("b", &dir.join("b"))),
    ];
    let session = TestSession::new();
    let first = discover(&session, &both).unwrap();
    let (old_a, old_b) = (candidate_ids(&first, "a"), candidate_ids(&first, "b"));

    let merged = scan(&session, &["a"], &one("a", &dir.join("a"))).unwrap();
    assert_eq!(category_ids(&merged), ["a", "b"], "a keeps its place");
    let new_a = candidate_ids(&merged, "a");
    assert_ne!(old_a, new_a, "IDs are never reused");
    assert_eq!(candidate_ids(&merged, "b"), old_b, "b is untouched");
    assert_eq!(merged.categories[1], first.categories[1]);

    assert!(invalid(session.clean_preview(old_a)));
    assert!(session.clean_preview(old_b).is_ok());
    assert!(session.clean_preview(new_a).is_ok());
    remove(&dir);
}

#[test]
fn a_new_category_joins_the_merged_set() {
    let dir = fixture_dir("join", &["x"], 4);
    let other = fixture_dir("join-other", &["y"], 4);
    let session = TestSession::new();
    let first = scan(&session, &["a"], &one("a", &dir)).unwrap();
    let merged = scan(&session, &["b"], &one("b", &other)).unwrap();
    assert_eq!(category_ids(&merged), ["a", "b"]);
    assert_eq!(merged.categories[0], first.categories[0]);
    assert_ne!(
        candidate_ids(&merged, "a")[0],
        candidate_ids(&merged, "b")[0],
        "IDs stay unique across scans"
    );
    remove(&dir);
    remove(&other);
}

#[test]
fn a_category_the_scan_covered_but_did_not_report_is_dropped() {
    let dir = fixture_dir("dropped", &["x"], 4);
    let session = TestSession::new();
    let first = scan(&session, &["a"], &one("a", &dir)).unwrap();
    let merged = scan(&session, &["a"], &[]).unwrap();
    assert!(merged.categories.is_empty());
    assert!(invalid(session.clean_preview(ids(&first))));
    remove(&dir);
}

#[test]
fn a_category_reported_skipped_replaces_its_earlier_result() {
    struct Running;
    impl AppProbe for Running {
        fn probe(&self, _: &str) -> std::result::Result<bool, String> {
            Ok(true)
        }
    }
    let dir = fixture_dir("skipped", &["x"], 4);
    let session = TestSession::new();
    let first = scan(&session, &["a"], &one("a", &dir)).unwrap();
    let mut gated = Fixture::new("a", &dir);
    gated.app = Some("Gate");
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(gated)];
    let again = session
        .discover_clean(
            covering(&["a"]),
            &providers,
            &Running,
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap();
    assert!(matches!(
        again.categories[0].status,
        FfiCategoryStatus::AppRunning { .. }
    ));
    assert!(invalid(session.clean_preview(ids(&first))));
    remove(&dir);
}

#[test]
fn a_cancelled_scan_keeps_everything() {
    let dir = fixture_dir("cancelled", &["x", "y"], 4);
    let session = TestSession::new();
    let first = scan(&session, &["logs"], &one("logs", &dir)).unwrap();
    let token = CancellationToken::default();
    token.cancel();
    for options in [covering(&["logs"]), CleanOptions::default()] {
        let cancelled = session.discover_clean(
            options,
            &one("logs", &dir),
            &NotRunning,
            &token,
            &Progress::default(),
        );
        assert!(matches!(cancelled, Err(FfiError::Cancelled)));
    }
    assert!(
        session.clean_preview(ids(&first)).is_ok(),
        "IDs still valid"
    );
    remove(&dir);
}

#[test]
fn a_cancelled_scan_does_not_spend_a_preview() {
    let dir = fixture_dir("cancelled-preview", &["x"], 4);
    let session = TestSession::new();
    let first = scan(&session, &["logs"], &one("logs", &dir)).unwrap();
    let preview = session.clean_preview(ids(&first)).unwrap();
    let token = CancellationToken::default();
    token.cancel();
    let cancelled = session.discover_clean(
        covering(&["logs"]),
        &one("logs", &dir),
        &NotRunning,
        &token,
        &Progress::default(),
    );
    assert!(matches!(cancelled, Err(FfiError::Cancelled)));
    let trash = FakeTrash::default();
    let run = execute(
        &session,
        &preview.preview_id,
        &dir,
        &trash,
        &CancellationToken::default(),
    );
    assert!(run.is_ok(), "{run:?}");
    remove(&dir);
}

/// Records the apps a run asks about.
#[derive(Default)]
struct RecordingProbe(Mutex<Vec<String>>);

impl AppProbe for RecordingProbe {
    fn probe(&self, app: &str) -> std::result::Result<bool, String> {
        self.0.lock().unwrap().push(app.to_string());
        Ok(false)
    }
}

#[test]
fn each_category_keeps_the_options_it_was_scanned_with() {
    let dir = fixture_dir("options", &["x"], 4);
    let other = fixture_dir("options-other", &["y"], 4);
    let session = TestSession::new();
    let with = |category: &str, idle_days| CleanOptions {
        idle_days,
        ..covering(&[category])
    };
    // The guard each provider carries depends on the options it is built from.
    let build = |idle_days: u64| -> Vec<Box<dyn CleanProvider>> {
        let guard = |a: &'static str, b: &'static str| if idle_days == 10 { a } else { b };
        let mut a = Fixture::new("a", &dir);
        a.app = Some(guard("A10", "A20"));
        let mut b = Fixture::new("b", &other);
        b.app = Some(guard("B10", "B20"));
        vec![Box::new(a), Box::new(b)]
    };
    let first = discover_with(&session, &one("a", &dir), with("a", 10)).unwrap();
    let merged = discover_with(&session, &one("b", &other), with("b", 20)).unwrap();
    assert_eq!(first.categories.len(), 1);
    let preview = session.clean_preview(ids(&merged)).unwrap();

    let asked = Mutex::new(Vec::new());
    let factory = |options: &CleanOptions| {
        asked.lock().unwrap().push(options.idle_days);
        build(options.idle_days)
    };
    let probe = RecordingProbe::default();
    let report = session
        .execute_clean(
            &preview.preview_id,
            &factory,
            &probe,
            &FakeTrash::default(),
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap();
    assert_eq!(report.moved_count, 2);
    let mut asked = asked.into_inner().unwrap();
    asked.sort();
    assert_eq!(asked, [10, 20], "one provider build per scan's options");
    let mut apps = probe.0.into_inner().unwrap();
    apps.sort();
    apps.dedup();
    assert_eq!(apps, ["A10", "B20"], "a is judged by 10 days, b by 20");
    remove(&dir);
    remove(&other);
}

#[test]
fn a_block_shared_across_categories_scanned_separately_is_counted_once() {
    let dir = fixture_dir("shared-block", &[], 0);
    for name in ["logs", "targets"] {
        fs::create_dir(dir.join(name)).unwrap();
        fs::write(dir.join(name).join("own"), [1u8; 9000]).unwrap();
    }
    fs::write(dir.join("logs/linked"), [2u8; 50_000]).unwrap();
    fs::hard_link(dir.join("logs/linked"), dir.join("targets/linked")).unwrap();
    let session = TestSession::new();
    let permanent = |ids| session.clean_preview_with_mode(ids, FfiDeleteMode::Permanent);

    let logs = scan(
        &session,
        &["user-logs"],
        &one("user-logs", &dir.join("logs")),
    )
    .unwrap();
    let own = allocated(&dir.join("logs/own"));
    let linked = allocated(&dir.join("logs/linked"));
    assert_eq!(permanent(ids(&logs)).unwrap().bytes_to_free, own, "alone");

    let targets = &one("rust-targets", &dir.join("targets"));
    let merged = scan(&session, &["rust-targets"], targets).unwrap();
    let both = permanent(ids(&merged)).unwrap();
    assert_eq!(
        both.bytes_to_free,
        2 * own + linked,
        "the earlier category's block joins the later one's"
    );
    let logs_only = permanent(candidate_ids(&merged, "user-logs")).unwrap();
    assert_eq!(logs_only.bytes_to_free, own, "alone it still frees no more");
    remove(&dir);
}

#[test]
fn an_overlapping_path_counts_once_after_a_merge() {
    let dir = fixture_dir("overlap-merge", &[], 0);
    fs::create_dir(dir.join("Caches")).unwrap();
    fs::write(dir.join("Caches/x"), b"12").unwrap();
    let mut browser = Fixture::new("browser", &dir.join("Caches"));
    browser.extra = vec![dir.join("Caches")];
    let session = TestSession::new();
    scan(&session, &["caches"], &one("caches", &dir)).unwrap();
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(browser)];
    let merged = scan(&session, &["browser"], &providers).unwrap();

    let preview = session.clean_preview(ids(&merged)).unwrap();
    assert_eq!(preview.items.len(), 1);
    assert_eq!(preview.bytes_selected, allocated(&dir.join("Caches/x")));
    assert_eq!(preview.excluded.len(), 2, "the duplicate and the child");
    remove(&dir);
}

#[test]
fn a_review_item_in_another_category_still_blocks_its_parent_after_a_merge() {
    let dir = review_fixture("review-merge");
    let session = TestSession::new();
    scan(&session, &["logs"], &one("logs", &dir.join("Logs"))).unwrap();
    let mut jetbrains = Fixture::new("jetbrains", &dir.join("Logs/JetBrains"));
    jetbrains.risk = RiskLevel::Review;
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(jetbrains)];
    let merged = scan(&session, &["jetbrains"], &providers).unwrap();

    let preview = session
        .clean_preview(candidate_ids(&merged, "logs"))
        .unwrap();
    assert!(preview
        .excluded
        .iter()
        .any(|e| matches!(e.reason, FfiExclusionReason::CoversUnselectedReview { .. })));
    remove(&dir);
}

#[test]
fn a_preview_of_untouched_categories_survives_a_rescan_of_another() {
    let dir = fixture_dir("survives", &["x"], 4);
    let other = fixture_dir("survives-other", &["y"], 4);
    let session = TestSession::new();
    scan(&session, &["logs"], &one("logs", &dir)).unwrap();
    let merged = scan(&session, &["other"], &one("other", &other)).unwrap();
    let preview = session
        .clean_preview(candidate_ids(&merged, "logs"))
        .unwrap();

    scan(&session, &["other"], &one("other", &other)).unwrap();
    let trash = FakeTrash::default();
    let report = execute(
        &session,
        &preview.preview_id,
        &dir,
        &trash,
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(outcomes(&report), vec![FfiItemOutcome::MovedToTrash]);
    remove(&dir);
    remove(&other);
}

#[test]
fn a_preview_of_a_rescanned_category_is_refused_unspent() {
    let dir = fixture_dir("stale", &["x"], 4);
    let other = fixture_dir("stale-other", &["y"], 4);
    let session = TestSession::new();
    scan(&session, &["logs"], &one("logs", &dir)).unwrap();
    let merged = scan(&session, &["other"], &one("other", &other)).unwrap();
    let preview = session.clean_preview(ids(&merged)).unwrap();

    scan(&session, &["other"], &one("other", &other)).unwrap();
    let trash = FakeTrash::default();
    let token = CancellationToken::default();
    let run = execute(&session, &preview.preview_id, &dir, &trash, &token);
    assert!(invalid(run));
    assert!(trash.steps().is_empty(), "nothing moved");
    remove(&dir);
    remove(&other);
}

#[test]
fn a_new_candidate_under_a_previewed_path_makes_the_preview_stale() {
    let dir = fixture_dir("grown", &[], 0);
    fs::create_dir(dir.join("Caches")).unwrap();
    fs::write(dir.join("Caches/x"), b"12").unwrap();
    let session = TestSession::new();
    let first = scan(&session, &["caches"], &one("caches", &dir)).unwrap();
    let preview = session.clean_preview(ids(&first)).unwrap();

    // Another category now lists something inside the previewed folder: its
    // guards were never part of what the preview checked.
    scan(&session, &["browser"], &one("browser", &dir.join("Caches"))).unwrap();
    let trash = FakeTrash::default();
    let token = CancellationToken::default();
    let providers = |_: &CleanOptions| one("caches", &dir);
    let run = session.execute_clean(
        &preview.preview_id,
        &providers,
        &NotRunning,
        &trash,
        &token,
        &Progress::default(),
    );
    assert!(invalid(run));
    assert!(trash.steps().is_empty());
    remove(&dir);
}

#[test]
fn an_empty_list_still_scans_all_and_replaces_all() {
    let dir = fixture_dir("replace-all", &["x"], 4);
    let other = fixture_dir("replace-all-other", &["y"], 4);
    let both: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(Fixture::new("a", &dir)),
        Box::new(Fixture::new("b", &other)),
    ];
    let session = TestSession::new();
    let first = discover(&session, &both).unwrap();

    let again = discover(&session, &one("b", &other)).unwrap();
    assert_eq!(category_ids(&again), ["b"], "a is gone, in scan order");
    assert!(invalid(session.clean_preview(ids(&first))));
    assert!(session.clean_preview(ids(&again)).is_ok());

    let reversed: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(Fixture::new("b", &other)),
        Box::new(Fixture::new("a", &dir)),
    ];
    let order = discover(&session, &reversed).unwrap();
    assert_eq!(category_ids(&order), ["b", "a"]);
    remove(&dir);
    remove(&other);
}
