//! Saved "always keep" rules in the projection and the preview: what is held
//! back, why, and what stays. Temp stores, temp dirs and `FakeTrash` only.

use tiny_core::clean::protection::RuleScope;

use super::*;

/// `logs` lists `dir/*` (here `d` and `a`); `inner` lists `dir/d/*` (`x`, `y`).
fn nested_providers(dir: &Path) -> Vec<Box<dyn CleanProvider>> {
    vec![
        Box::new(Fixture::new("logs", dir)),
        Box::new(Fixture::new("inner", &dir.join("d"))),
    ]
}

fn nested_fixture(label: &str) -> PathBuf {
    let dir = fixture_dir(label, &["a"], 4);
    std::fs::create_dir(dir.join("d")).unwrap();
    std::fs::write(dir.join("d/x"), b"123").unwrap();
    std::fs::write(dir.join("d/y"), b"12345").unwrap();
    dir
}

fn exclusion_of<'a>(preview: &'a FfiPreview, name: &str) -> &'a FfiExclusionReason {
    let found = preview.excluded.iter().find(|e| e.path.ends_with(name));
    &found
        .unwrap_or_else(|| panic!("{name} is not excluded: {:?}", preview.excluded))
        .reason
}

fn kept_paths(preview: &FfiPreview) -> Vec<String> {
    let name = |p: &String| p.rsplit('/').next().unwrap().to_string();
    preview.items.iter().map(|i| name(&i.path)).collect()
}

#[test]
fn reading_the_rules_and_previewing_never_create_files() {
    let dir = nested_fixture("no-files");
    let session = TestSession::new();
    let discovery = discover(&session, &nested_providers(&dir)).unwrap();
    session.clean_preview(ids(&discovery)).unwrap();
    let protection = session.clean_protection().unwrap();
    assert_eq!(revision(&protection), 0);
    assert!(rules(&protection).is_empty());
    assert!(
        !session.dir.exists(),
        "a read-only scan, preview or load must not initialize the store"
    );
    remove(&dir);
}

#[test]
fn the_projection_lists_keep_scopes_and_never_empty_rules_for_a_bad_store() {
    let dir = nested_fixture("scopes");
    std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();
    let mut docker = Fixture::new("docker", &dir.join("none"));
    docker.desktop = false;
    docker.extra = vec![PathBuf::from("<docker:images>")];
    let mut providers = nested_providers(&dir);
    providers.push(Box::new(docker));
    let session = TestSession::new();
    let discovery = discover(&session, &providers).unwrap();
    let before = session.clean_protection().unwrap();
    assert_eq!(
        before.discovery_id.as_deref(),
        Some(&*discovery.discovery_id)
    );
    let scope = |name| candidate_protection(&before, &discovery, name).keep_scope;
    assert_eq!(scope("/a"), Some(FfiProtectionScope::File));
    assert_eq!(scope("/d"), Some(FfiProtectionScope::Directory));
    assert_eq!(
        scope("/link"),
        None,
        "a symlink is never kept by its target"
    );
    assert_eq!(scope("<docker:images>"), None, "report-only and not a path");

    keep_candidate(&session, &id_of(&discovery, "/a"));
    std::fs::write(session.store().dir().join("rules.json"), b"{ not json").unwrap();
    let broken = session.clean_protection().unwrap();
    assert!(matches!(
        &broken.status,
        FfiProtectionStatus::Unavailable { detail } if detail.contains("malformed")
    ));
    assert_eq!(
        broken.location,
        session.store().dir().to_string_lossy().into_owned()
    );
    assert_eq!(
        broken.discovery_id.as_deref(),
        Some(&*discovery.discovery_id)
    );
    // Discovery stays inspectable; the preview refuses to authorize anything.
    let refused = session.clean_preview(vec![id_of(&discovery, "/a")]);
    assert!(
        matches!(refused, Err(FfiError::ProtectionUnavailable { .. })),
        "{refused:?}"
    );
    remove(&dir);
}

#[test]
fn a_direct_rule_holds_one_path_and_leaves_its_siblings() {
    let dir = nested_fixture("direct");
    let session = TestSession::new();
    let discovery = discover(&session, &nested_providers(&dir)).unwrap();
    let after = keep_candidate(&session, &id_of(&discovery, "/a"));
    let held = candidate_protection(&after, &discovery, "/a");
    assert!(matches!(
        held.guards.as_slice(),
        [FfiProtectionGuard::Kept { rule_id, .. }] if rule_id == "r1"
    ));
    assert!(candidate_protection(&after, &discovery, "/y")
        .guards
        .is_empty());

    let preview = session.clean_preview(ids(&discovery)).unwrap();
    assert!(matches!(
        exclusion_of(&preview, "/a"),
        FfiExclusionReason::Protected {
            held_by: FfiProtectionGuard::Kept { .. }
        }
    ));
    let mut moved = kept_paths(&preview);
    moved.sort();
    assert_eq!(moved, ["d"], "x and y move with d; a is held back");
    assert_eq!(preview.bytes_selected, 8, "d's own measured size only");
    remove(&dir);
}

#[test]
fn a_rule_inside_a_selected_folder_holds_the_folder_but_not_its_other_selection() {
    let dir = nested_fixture("ancestor");
    let session = TestSession::new();
    let discovery = discover(&session, &nested_providers(&dir)).unwrap();
    let after = keep_candidate(&session, &id_of(&discovery, "/x"));
    let parent = candidate_protection(&after, &discovery, "/d");
    assert!(matches!(
        parent.guards.as_slice(),
        [FfiProtectionGuard::HoldsKept { rule_path, .. }] if rule_path.ends_with("/d/x")
    ));

    let ids = vec![id_of(&discovery, "/d"), id_of(&discovery, "/y")];
    let preview = session.clean_preview(ids).unwrap();
    assert!(matches!(
        exclusion_of(&preview, "/d"),
        FfiExclusionReason::Protected {
            held_by: FfiProtectionGuard::HoldsKept { .. }
        }
    ));
    assert_eq!(kept_paths(&preview), ["y"], "the safe sibling stays");
    assert_eq!(
        preview.bytes_selected, 5,
        "no parent-minus-child arithmetic"
    );
    assert!(preview.items[0].covers.is_empty());
    remove(&dir);
}

#[test]
fn a_folder_rule_holds_everything_inside_it() {
    let dir = nested_fixture("descendants");
    let session = TestSession::new();
    let discovery = discover(&session, &nested_providers(&dir)).unwrap();
    let after = keep_candidate(&session, &id_of(&discovery, "/d"));
    for name in ["/d", "/x", "/y"] {
        let guards = candidate_protection(&after, &discovery, name).guards;
        assert!(
            matches!(guards.as_slice(), [FfiProtectionGuard::Kept { .. }]),
            "{name}: {guards:?}"
        );
    }
    let preview = session.clean_preview(ids(&discovery)).unwrap();
    assert_eq!(kept_paths(&preview), ["a"]);
    assert_eq!(preview.excluded.len(), 3);
    remove(&dir);
}

#[test]
fn a_rule_the_scan_never_found_still_holds_its_ancestor() {
    let dir = nested_fixture("undiscovered");
    let session = TestSession::new();
    let discovery = discover(&session, &nested_providers(&dir)).unwrap();
    {
        let store = session.store();
        let mut lease = store.lease().unwrap();
        let ghost = dir.join("d/inner/precious");
        lease.add(0, &ghost, RuleScope::File).unwrap();
    }
    let protection = session.clean_protection().unwrap();
    let listed = rules(&protection);
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].path_exists, "a missing path stays saved");
    assert!(matches!(
        candidate_protection(&protection, &discovery, "/d")
            .guards
            .as_slice(),
        [FfiProtectionGuard::HoldsKept { .. }]
    ));
    let preview = session.clean_preview(vec![id_of(&discovery, "/d")]);
    assert!(
        preview.unwrap().items.is_empty(),
        "nothing authorized: the only selection carries the kept path"
    );
    remove(&dir);
}

#[test]
fn one_rule_holds_the_same_path_in_every_category() {
    let dir = nested_fixture("equal-path");
    let providers = || -> Vec<Box<dyn CleanProvider>> {
        vec![
            Box::new(Fixture::new("logs", &dir)),
            Box::new(Fixture::new("also-logs", &dir)),
        ]
    };
    let session = TestSession::new();
    let discovery = discover(&session, &providers()).unwrap();
    let a_ids: Vec<_> = discovery
        .categories
        .iter()
        .flat_map(|c| c.candidates.iter().filter(|c| c.path.ends_with("/a")))
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(a_ids.len(), 2);
    let after = keep_candidate(&session, &a_ids[0]);
    for id in &a_ids {
        let FfiProtectionStatus::Available { candidates, .. } = &after.status else {
            panic!("available");
        };
        let entry = candidates.iter().find(|c| &c.candidate_id == id).unwrap();
        assert!(matches!(
            entry.guards.as_slice(),
            [FfiProtectionGuard::Kept { .. }]
        ));
    }
    let preview = session.clean_preview(a_ids).unwrap();
    assert!(preview.items.is_empty());
    assert!(preview
        .excluded
        .iter()
        .all(|e| matches!(e.reason, FfiExclusionReason::Protected { .. })));
    remove(&dir);
}

#[test]
fn review_candidates_are_kept_without_becoming_a_review_reason() {
    let dir = review_fixture("keep-review");
    let session = TestSession::new();
    let discovery = discover(&session, &review_inside_safe(&dir)).unwrap();
    let review = candidate_ids(&discovery, "jetbrains");
    let safe = candidate_ids(&discovery, "logs");
    keep_candidate(&session, &review[0]);

    // Selecting only the safe parent: the rule, not an unselected Review item, holds it.
    let preview = session.clean_preview(safe.clone()).unwrap();
    assert!(matches!(
        exclusion_of(&preview, "JetBrains"),
        FfiExclusionReason::Protected { .. }
    ));
    // Selecting the Review child too cannot override a saved rule.
    let all = session
        .clean_preview(safe.into_iter().chain(review).collect())
        .unwrap();
    assert_eq!(kept_paths(&all), ["other"]);
    assert!(all
        .excluded
        .iter()
        .all(|e| matches!(e.reason, FfiExclusionReason::Protected { .. })));
    remove(&dir);
}

#[test]
fn tinys_own_storage_is_never_offered_for_cleanup() {
    let dir = fixture_dir("storage", &["a"], 1);
    let session = TestSession::new();
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("logs", &dir))];
    let discovery = discover(&session, &providers).unwrap();
    keep_candidate(&session, &id_of(&discovery, "/a"));
    // Once saved, the store sits in the session's temp dir; list that dir.
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("tiny", &session.dir))];
    let discovery = discover(&session, &providers).unwrap();
    let protection = session.clean_protection().unwrap();
    let holds = candidate_protection(&protection, &discovery, "clean-protection");
    assert_eq!(holds.guards, [FfiProtectionGuard::TinyStorage]);
    let preview = session.clean_preview(vec![id_of(&discovery, "clean-protection")]);
    assert!(preview.unwrap().items.is_empty());
    remove(&dir);
}
