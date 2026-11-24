//! Keeping a path the user picked, in a scan or not: what is saved, what is
//! refused, and that the write obeys the same revision rules as `clean_keep`.
//! Temp stores and temp dirs only.

use super::*;

fn providers(dir: &Path) -> Vec<Box<dyn CleanProvider>> {
    vec![Box::new(Fixture::new("logs", dir))]
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Loads the rules, then keeps `path` at the revision just loaded.
fn keep_path(session: &TinySession, path: &Path) -> Result<FfiProtection, FfiError> {
    let revision = revision(&session.clean_protection().unwrap());
    session.clean_keep_path(text(path), revision)
}

fn refused(result: &Result<FfiProtection, FfiError>, words: &str) -> bool {
    matches!(result, Err(FfiError::InvalidInput { detail }) if detail.contains(words))
}

#[test]
fn a_picked_folder_and_file_are_kept_without_a_scan() {
    let dir = fixture_dir("picked", &["a"], 1);
    std::fs::create_dir(dir.join("folder")).unwrap();
    let session = TestSession::new();

    let folder = keep_path(&session, &dir.join("folder")).unwrap();
    assert_eq!(folder.discovery_id, None, "no scan ran");
    let saved = rules(&folder);
    assert_eq!((revision(&folder), saved.len()), (1, 1));
    assert_eq!(saved[0].path, text(&dir.join("folder")));
    assert_eq!(saved[0].scope, FfiProtectionScope::Directory);
    assert!(saved[0].path_exists);

    let file = session.clean_keep_path(text(&dir.join("a")), 1).unwrap();
    let saved = rules(&file);
    assert_eq!((revision(&file), saved.len()), (2, 2));
    assert_eq!(saved[1].scope, FfiProtectionScope::File);

    let again = session.clean_keep_path(text(&dir.join("a")), 2);
    assert!(refused(&again, "already kept"), "{again:?}");
    remove(&dir);
}

#[test]
fn a_picked_path_holds_the_candidates_a_later_scan_finds() {
    let dir = fixture_dir("picked-holds", &["a", "b"], 1);
    let session = TestSession::new();
    keep_path(&session, &dir.join("a")).unwrap();

    let discovery = discover(&session, &providers(&dir)).unwrap();
    let protection = session.clean_protection().unwrap();
    let held = candidate_protection(&protection, &discovery, "/a");
    assert!(
        matches!(held.guards.first(), Some(FfiProtectionGuard::Kept { rule_id, .. }) if rule_id == "r1"),
        "{held:?}"
    );
    assert!(candidate_protection(&protection, &discovery, "/b")
        .guards
        .is_empty());
    remove(&dir);
}

#[test]
fn a_path_picked_through_a_symlink_is_kept_where_it_really_is() {
    let dir = fixture_dir("picked-through-link", &[], 1);
    std::fs::create_dir_all(dir.join("real").join("project")).unwrap();
    std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
    let session = TestSession::new();

    let kept = keep_path(&session, &dir.join("alias").join("project")).unwrap();
    assert_eq!(
        rules(&kept)[0].path,
        text(&dir.join("real").join("project")),
        "a rule spelled through the link would hold nothing a scan finds"
    );
    let same = session.clean_keep_path(text(&dir.join("real").join("project")), 1);
    assert!(refused(&same, "already kept"), "{same:?}");
    remove(&dir);
}

#[test]
fn a_picked_path_holds_a_candidate_the_scan_spells_through_a_symlink() {
    let dir = fixture_dir("picked-scan-link", &[], 1);
    let real = dir.join("real");
    std::fs::create_dir(&real).unwrap();
    for name in ["a", "b"] {
        std::fs::write(real.join(name), b"x").unwrap();
    }
    std::os::unix::fs::symlink(&real, dir.join("alias")).unwrap();
    let session = TestSession::new();
    keep_path(&session, &real.join("a")).unwrap();

    // A provider builds its root from the system's own spelling, links and
    // all (the Font cache is `/var/folders/…`), and discovery keeps it.
    let scanned = dir.join("alias");
    let discovery = discover(&session, &providers(&scanned)).unwrap();
    assert!(id_of(&discovery, "/alias/a").starts_with('d'));
    let protection = session.clean_protection().unwrap();
    let held = candidate_protection(&protection, &discovery, "/a");
    assert!(
        matches!(held.guards.first(), Some(FfiProtectionGuard::Kept { rule_id, .. }) if rule_id == "r1"),
        "the rule names the real location and the scan another spelling: {held:?}"
    );
    assert!(candidate_protection(&protection, &discovery, "/b")
        .guards
        .is_empty());

    let preview = session.clean_preview(ids(&discovery)).unwrap();
    let moving: Vec<&str> = preview.items.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(
        moving,
        [text(&scanned.join("b"))],
        "only the item no rule holds"
    );
    assert!(
        matches!(
            preview.excluded.as_slice(),
            [FfiPreviewExclusion {
                reason: FfiExclusionReason::Protected { .. },
                ..
            }]
        ),
        "{:?}",
        preview.excluded
    );
    remove(&dir);
}

#[test]
fn one_item_gets_one_rule_whatever_spelling_asks() {
    let dir = fixture_dir("picked-twice", &[], 1);
    let real = dir.join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("a"), b"x").unwrap();
    std::os::unix::fs::symlink(&real, dir.join("alias")).unwrap();
    let session = TestSession::new();

    // The scan row keeps the scan's own spelling, through the link.
    let discovery = discover(&session, &providers(&dir.join("alias"))).unwrap();
    let kept = keep_candidate(&session, &id_of(&discovery, "/alias/a"));
    assert_eq!(rules(&kept)[0].path, text(&dir.join("alias").join("a")));

    let again = session.clean_keep_path(text(&real.join("a")), 1);
    assert!(refused(&again, "already kept"), "{again:?}");
    let through = session.clean_keep_path(text(&dir.join("alias").join("a")), 1);
    assert!(refused(&through, "already kept"), "{through:?}");
    assert_eq!(rules(&session.clean_protection().unwrap()).len(), 1);
    remove(&dir);
}

#[cfg(target_os = "macos")]
#[test]
fn the_data_volume_and_another_case_do_not_get_past_the_home_refusal() {
    let dir = fixture_dir("picked-home-spellings", &[], 1);
    let home = dir.join("Users").join("someone");
    std::fs::create_dir_all(&home).unwrap();
    let session = TestSession::new();
    session.clean_state().home_inputs = Some((Some(home.clone()), Some(home.clone())));
    session.clean_protection().unwrap();

    // The long spelling of the data volume holds the home folder too.
    for above in ["/System/Volumes/Data", "/System/Volumes", "/System"] {
        let result = session.clean_keep_path(above.into(), 0);
        assert!(
            refused(&result, "an ancestor of the home folder"),
            "{above}: {result:?}"
        );
    }
    // Where the volume ignores case, another case is the same folder.
    if dir.join("USERS").exists() {
        let shouted = session.clean_keep_path(text(&dir.join("Users").join("SOMEONE")), 0);
        assert!(refused(&shouted, "the home folder"), "{shouted:?}");
        let above = session.clean_keep_path(text(&dir.join("USERS")), 0);
        assert!(
            refused(&above, "an ancestor of the home folder"),
            "{above:?}"
        );
    }
    assert!(!session.store().dir().exists(), "nothing was written");
    remove(&dir);
}

#[cfg(target_os = "macos")]
#[test]
fn a_path_on_the_data_volume_is_kept_under_its_short_spelling() {
    let dir = fixture_dir("picked-data-volume", &["a"], 1);
    let long = Path::new("/System/Volumes/Data").join(dir.strip_prefix("/").unwrap());
    if !long.join("a").exists() {
        // The temp dir is not on the data volume here: nothing to shorten.
        remove(&dir);
        return;
    }
    let session = TestSession::new();
    let kept = keep_path(&session, &long.join("a")).unwrap();
    assert_eq!(rules(&kept)[0].path, text(&dir.join("a")));
    remove(&dir);
}

#[test]
fn paths_a_rule_cannot_name_are_refused_without_writing() {
    let dir = fixture_dir("picked-refused", &["a"], 1);
    std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();
    let session = TestSession::new();
    session.clean_protection().unwrap();

    let cases: [(String, &str); 7] = [
        ("relative/path".into(), "not absolute"),
        (format!("{}/", text(&dir)), "canonical form"),
        (format!("{}/../x", text(&dir)), "canonical form"),
        ("/".into(), "root"),
        (text(&dir.join("missing")), "nothing is at this path"),
        (
            text(&dir.join("missing").join("deeper")),
            "nothing is at this path",
        ),
        (text(&dir.join("link")), "link or a special file"),
    ];
    for (path, words) in cases {
        let result = session.clean_keep_path(path.clone(), 0);
        assert!(refused(&result, words), "{path}: {result:?}");
    }
    assert!(!session.store().dir().exists(), "nothing was written");
    remove(&dir);
}

#[test]
fn the_home_folder_and_the_folders_holding_it_are_refused() {
    let dir = fixture_dir("picked-home", &[], 1);
    let home = dir.join("Users").join("someone");
    std::fs::create_dir_all(home.join("Projects")).unwrap();
    let session = TestSession::new();
    session.clean_state().home_inputs = Some((Some(home.clone()), Some(home.clone())));
    session.clean_protection().unwrap();

    let at_home = session.clean_keep_path(text(&home), 0);
    assert!(refused(&at_home, "the home folder"), "{at_home:?}");
    let above = session.clean_keep_path(text(&dir.join("Users")), 0);
    assert!(
        refused(&above, "an ancestor of the home folder"),
        "{above:?}"
    );
    assert!(!session.store().dir().exists(), "nothing was written");

    let inside = session
        .clean_keep_path(text(&home.join("Projects")), 0)
        .unwrap();
    assert_eq!(rules(&inside).len(), 1);
    remove(&dir);
}

#[test]
fn a_picked_path_obeys_the_revision_and_drops_the_stored_preview() {
    let dir = fixture_dir("picked-revision", &["a", "b"], 1);
    let session = TestSession::new();

    // Never loaded: a write cannot name a revision it never saw.
    let unloaded = session.clean_keep_path(text(&dir.join("a")), 0);
    assert!(
        matches!(unloaded, Err(FfiError::ProtectionChanged)),
        "{unloaded:?}"
    );

    // Another window saved a rule since this one read them.
    session.clean_protection().unwrap();
    keep_path(&session.sibling(), &dir.join("b")).unwrap();
    let stale = session.clean_keep_path(text(&dir.join("a")), 0);
    assert!(
        matches!(stale, Err(FfiError::ProtectionChanged)),
        "{stale:?}"
    );

    let preview = preview_all(&session, &dir);
    keep_path(&session, &dir.join("a")).unwrap();
    let trash = FakeTrash::default();
    let run = execute(
        &session,
        &preview.preview_id,
        &dir,
        &trash,
        &CancellationToken::default(),
    );
    assert!(
        matches!(run, Err(FfiError::PreviewInvalid { .. })),
        "a preview built for the old rules must not run: {run:?}"
    );
    assert!(dir.join("a").exists() && dir.join("b").exists());
    remove(&dir);
}
