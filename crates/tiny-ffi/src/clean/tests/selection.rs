//! Nested selections: the app gate and PC-C3 hold for paths a selected
//! ancestor carries along. Temp dirs and `FakeTrash` only.

use super::*;

/// Runs discover → preview(all) → execute for `providers` with Spotify running.
fn execute_with_spotify_running(
    providers: impl Fn() -> Vec<Box<dyn CleanProvider>>,
) -> FfiExecReport {
    struct SpotifyRunning;
    impl AppProbe for SpotifyRunning {
        fn probe(&self, name: &str) -> std::result::Result<bool, String> {
            Ok(name == "Spotify")
        }
    }
    let session = TestSession::new();
    let discovery = discover(&session, &providers()).unwrap();
    let preview = session.clean_preview(ids(&discovery)).unwrap();
    assert_eq!(preview.items.len(), 1, "{:?}", preview.items);
    let factory = |_: &CleanOptions| providers();
    session
        .execute_clean(
            &preview.preview_id,
            &factory,
            &SpotifyRunning,
            &FakeTrash::default(),
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap()
}

#[test]
fn a_duplicate_keeps_the_app_gate_of_either_category_in_any_order() {
    let dir = fixture_dir("spotify-dup", &[], 0);
    std::fs::create_dir(dir.join("com.spotify.client")).unwrap();
    // user-caches lists the folder; streaming-caches finds it gated on Spotify.
    let gated = |root: &Path| {
        let mut streaming = Fixture::new("streaming", &root.join("none"));
        streaming.app = Some("Spotify");
        streaming.extra = vec![root.join("com.spotify.client")];
        Box::new(streaming) as Box<dyn CleanProvider>
    };
    for gated_first in [true, false] {
        let root = dir.clone();
        let providers = || {
            let caches: Box<dyn CleanProvider> = Box::new(Fixture::new("caches", &root));
            if gated_first {
                vec![gated(&root), caches]
            } else {
                vec![caches, gated(&root)]
            }
        };
        let report = execute_with_spotify_running(providers);
        assert_eq!(
            outcomes(&report),
            vec![FfiItemOutcome::Skipped {
                reason: FfiSkipReason::AppRunning,
                detail: Some("Spotify".into())
            }],
            "gated_first = {gated_first}"
        );
    }
    remove(&dir);
}

#[test]
fn an_unselected_overlapping_candidate_still_gates_the_move() {
    let dir = fixture_dir("spotify-unselected", &[], 0);
    std::fs::create_dir(dir.join("com.spotify.client")).unwrap();
    let root = dir.clone();
    let providers = move || -> Vec<Box<dyn CleanProvider>> {
        let mut streaming = Fixture::new("streaming", &root.join("none"));
        streaming.app = Some("Spotify");
        streaming.extra = vec![root.join("com.spotify.client")];
        vec![Box::new(Fixture::new("caches", &root)), Box::new(streaming)]
    };
    struct SpotifyRunning;
    impl AppProbe for SpotifyRunning {
        fn probe(&self, name: &str) -> std::result::Result<bool, String> {
            Ok(name == "Spotify")
        }
    }
    let session = TestSession::new();
    let discovery = discover(&session, &providers()).unwrap();
    // Select only the user-caches-like candidate, not the streaming one.
    let caches = discovery
        .categories
        .iter()
        .find(|c| c.id == "caches")
        .unwrap();
    let preview = session
        .clean_preview(vec![caches.candidates[0].id.clone()])
        .unwrap();
    assert!(preview.excluded.is_empty());
    let factory = |_: &CleanOptions| providers();
    let report = session
        .execute_clean(
            &preview.preview_id,
            &factory,
            &SpotifyRunning,
            &FakeTrash::default(),
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap();
    assert_eq!(
        outcomes(&report),
        vec![FfiItemOutcome::Skipped {
            reason: FfiSkipReason::AppRunning,
            detail: Some("Spotify".into())
        }]
    );
    remove(&dir);
}

#[test]
fn an_ancestor_selected_over_a_gated_child_keeps_the_child_gate() {
    let dir = fixture_dir("spotify-child", &[], 0);
    std::fs::create_dir_all(dir.join("Caches/com.spotify.client")).unwrap();
    let root = dir.clone();
    let providers = || -> Vec<Box<dyn CleanProvider>> {
        let mut ancestor = Fixture::new("library", &root.join("none"));
        ancestor.extra = vec![root.join("Caches")];
        let mut child = Fixture::new("streaming", &root.join("none"));
        child.app = Some("Spotify");
        child.extra = vec![root.join("Caches/com.spotify.client")];
        vec![Box::new(ancestor), Box::new(child)]
    };
    let report = execute_with_spotify_running(providers);
    assert_eq!(
        outcomes(&report),
        vec![FfiItemOutcome::Skipped {
            reason: FfiSkipReason::AppRunning,
            detail: Some("Spotify".into())
        }]
    );
    assert!(dir.join("Caches/com.spotify.client").exists());
    remove(&dir);
}

// ---------- PC-C3: review items need explicit selection ----------

#[test]
fn a_selected_item_never_carries_an_unselected_review_candidate() {
    let dir = review_fixture("review-inside");
    let session = TestSession::new();
    let discovery = discover(&session, &review_inside_safe(&dir)).unwrap();
    let safe = candidate_ids(&discovery, "logs");
    let review = candidate_ids(&discovery, "jetbrains");
    assert_eq!((safe.len(), review.len()), (2, 1));

    let preview = session.clean_preview(safe.clone()).unwrap();
    let kept: Vec<_> = preview.items.iter().map(|i| i.path.clone()).collect();
    assert_eq!(
        kept,
        vec![dir.join("Logs/other").to_string_lossy().to_string()]
    );
    let blocked = preview
        .excluded
        .iter()
        .find(|e| e.path.ends_with("JetBrains"))
        .expect("the review-carrying folder is excluded");
    assert_eq!(
        blocked.reason,
        FfiExclusionReason::CoversUnselectedReview {
            candidate_ids: review.clone()
        }
    );

    // Selecting the review child too is explicit consent: the folder is kept.
    let both = session
        .clean_preview(safe.iter().chain(&review).cloned().collect())
        .unwrap();
    assert!(both.items.iter().any(|i| i.path.ends_with("JetBrains")));
    remove(&dir);
}

#[test]
fn execute_refuses_a_preview_that_would_move_an_unselected_review_item() {
    let dir = review_fixture("review-execute");
    let session = TestSession::new();
    let discovery = discover(&session, &review_inside_safe(&dir)).unwrap();
    let review = candidate_ids(&discovery, "jetbrains");
    let all = ids(&discovery);
    let preview = session.clean_preview(all).unwrap();
    // Forge the stored selection, as a future bug might.
    session
        .clean_state()
        .preview
        .as_mut()
        .unwrap()
        .selected
        .remove(&review[0]);
    let root = dir.clone();
    let factory = move |_: &CleanOptions| review_inside_safe(&root);
    let trash = FakeTrash::default();
    let run = session.execute_clean(
        &preview.preview_id,
        &factory,
        &NotRunning,
        &trash,
        &CancellationToken::default(),
        &Progress::default(),
    );
    assert!(matches!(run, Err(FfiError::PreviewInvalid { .. })));
    assert!(trash.moved.lock().unwrap().is_empty());
    assert!(dir.join("Logs/JetBrains/Product").exists());
    remove(&dir);
}

#[test]
fn a_preview_item_lists_every_candidate_it_moves_with_selection() {
    let dir = review_fixture("covers");
    let mut inner = Fixture::new("inner", &dir.join("Logs/JetBrains"));
    inner.risk = RiskLevel::Safe;
    let providers: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(Fixture::new("logs", &dir.join("Logs"))),
        Box::new(inner),
    ];
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    // Tick the parent, untick its child: the child still moves with the parent.
    let preview = session
        .clean_preview(candidate_ids(&discovery, "logs"))
        .unwrap();
    let parent = preview
        .items
        .iter()
        .find(|i| i.path.ends_with("JetBrains"))
        .unwrap();
    let child = &candidate_ids(&discovery, "inner")[0];
    assert_eq!(
        parent.covers,
        vec![FfiCoveredCandidate {
            candidate_id: child.clone(),
            path: dir
                .join("Logs/JetBrains/Product")
                .to_string_lossy()
                .into_owned(),
            risk: FfiRisk::Safe,
            selected: false,
        }]
    );
    remove(&dir);
}
