use super::*;
use std::os::unix::fs::PermissionsExt;

fn temp() -> tempfile::TempDir {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn find<'a>(node: &'a SpaceNode, name: &str) -> &'a SpaceNode {
    node.children.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn lens_counts_nested_files_without_following_symlinks() {
    let dir = temp();
    let outside = temp();
    fs::write(dir.path().join("a"), b"abc").unwrap();
    fs::write(outside.path().join("b"), b"secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let result = scan(dir.path(), None, None).unwrap();
    assert_eq!(result.root.bytes, 3);
    assert_eq!(result.files_scanned, 1);
    assert!(!result.incomplete && !result.cancelled);
    assert!(scan(&dir.path().join("link"), None, None).is_err());
    assert!(validate_path(&dir.path().join("link/b")).is_err());
}

#[test]
fn omitted_children_keep_totals_and_are_summarized() {
    let dir = temp();
    let big = dir.path().join("big");
    fs::create_dir(&big).unwrap();
    // More than twice the cap, so the streaming trim runs as well as the final one.
    let count = MAX_CHILDREN * 2 + 50;
    for i in 0..count {
        fs::write(big.join(format!("f{i:03}")), vec![0u8; i + 1]).unwrap();
    }
    let result = scan(dir.path(), None, None).unwrap();
    let node = find(&result.root, "big");
    let total: u64 = (1..=count as u64).sum();
    assert_eq!(node.bytes, total);
    assert_eq!(node.children.len(), MAX_CHILDREN);
    assert_eq!(node.omitted_count, (count - MAX_CHILDREN) as u64);
    let retained: u64 = node.children.iter().map(|c| c.bytes).sum();
    assert_eq!(retained + node.omitted_bytes, node.bytes);
    assert_eq!(
        node.children[0].bytes, count as u64,
        "largest entries are kept"
    );
    assert!(!result.incomplete && result.depth_limited_count == 0);
}

#[test]
fn root_children_beyond_the_cap_are_summarized() {
    let dir = temp();
    for i in 0..MAX_CHILDREN + 5 {
        fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
    }
    let result = scan(dir.path(), None, None).unwrap();
    assert_eq!(result.root.bytes, (MAX_CHILDREN + 5) as u64);
    assert_eq!(result.root.children.len(), MAX_CHILDREN);
    assert_eq!(result.root.omitted_count, 5);
    assert_eq!(result.root.omitted_bytes, 5);
}

#[test]
fn depth_limited_folder_reports_what_it_hides() {
    let dir = temp();
    let deep = dir.path().join("a/b/c");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("file"), b"12345").unwrap();
    let result = scan(dir.path(), None, None).unwrap();
    let b = find(find(&result.root, "a"), "b");
    assert_eq!(b.bytes, 5);
    assert!(b.children.is_empty());
    assert_eq!((b.omitted_count, b.omitted_bytes), (1, 5));
    assert!(!result.incomplete && result.depth_limited_count == 0);
    let drilled = scan(&dir.path().join("a/b"), None, None).unwrap();
    assert_eq!(find(&drilled.root, "c").bytes, 5);
}

struct Restore(Vec<PathBuf>);
impl Drop for Restore {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        }
    }
}

fn lock(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
}

#[test]
fn unreadable_folder_is_a_lower_bound_not_zero() {
    let dir = temp();
    fs::write(dir.path().join("ok"), b"abc").unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("hidden"), b"secret").unwrap();
    let _restore = Restore(vec![locked.clone()]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let result = scan(dir.path(), None, None).unwrap();
    let node = find(&result.root, "locked");
    assert!(node.unreadable && result.root.unreadable && result.incomplete);
    assert_eq!(result.skipped_count, 1);
    assert_eq!(result.skipped_paths, vec![locked.display().to_string()]);
    assert_eq!(result.root.bytes, 3);
}

#[test]
fn folders_deeper_than_the_walk_limit_are_marked_not_empty() {
    let dir = temp();
    let mut deep = dir.path().to_path_buf();
    for _ in 0..MAX_DEPTH + 3 {
        deep.push("d");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("file"), b"x").unwrap();
    let result = scan(dir.path(), None, None).unwrap();
    assert!(result.incomplete && result.root.depth_limited);
    assert_eq!(result.depth_limited_count, 1);
    assert_eq!(result.files_scanned, 0);
}

#[test]
fn depth_stop_has_its_own_reason_and_stays_a_lower_bound() {
    let dir = temp();
    fs::write(dir.path().join("ok"), b"abc").unwrap();
    let deep = dir.path().join("a/b/c");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("file"), b"12345").unwrap();
    let result = scan_with_depth(dir.path(), None, None, 2).unwrap();
    let b = find(find(&result.root, "a"), "b");
    assert!(b.depth_limited && result.root.depth_limited);
    assert!(!b.unreadable && !result.root.unreadable);
    assert_eq!(result.depth_limited_count, 1);
    assert_eq!(
        result.skipped_count, 0,
        "not walked is not the same as skipped"
    );
    assert!(result.skipped_paths.is_empty());
    assert!(result.incomplete);
    assert_eq!(result.root.bytes, 3, "the hidden file is not counted");
}

#[test]
fn hard_links_count_once_inside_one_branch() {
    let dir = temp();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("orig"), vec![0u8; 5000]).unwrap();
    fs::hard_link(sub.join("orig"), sub.join("link")).unwrap();
    fs::write(sub.join("solo"), b"abc").unwrap();
    let result = scan(dir.path(), None, None).unwrap();
    let single = fs::symlink_metadata(sub.join("orig")).unwrap();
    assert_eq!(result.root.bytes, 5003);
    assert_eq!(
        result.root.allocated_bytes,
        allocated(&single) + allocated(&fs::symlink_metadata(sub.join("solo")).unwrap())
    );
    assert_eq!(result.files_scanned, 2);
}

#[test]
fn hard_links_count_once_across_parallel_branches() {
    let dir = temp();
    for branch in ["a", "b", "c"] {
        fs::create_dir(dir.path().join(branch)).unwrap();
    }
    fs::write(dir.path().join("a/orig"), vec![0u8; 5000]).unwrap();
    fs::hard_link(dir.path().join("a/orig"), dir.path().join("b/l1")).unwrap();
    fs::hard_link(dir.path().join("a/orig"), dir.path().join("c/l2")).unwrap();
    fs::hard_link(dir.path().join("a/orig"), dir.path().join("top")).unwrap();
    // Which branch gets the bytes is not deterministic, so only the root total is asserted.
    for _ in 0..20 {
        let result = scan(dir.path(), None, None).unwrap();
        assert_eq!(result.root.bytes, 5000);
        assert_eq!(result.files_scanned, 1);
    }
}

#[test]
fn single_link_files_are_never_deduplicated() {
    let dir = temp();
    fs::write(dir.path().join("x"), b"same").unwrap();
    fs::write(dir.path().join("y"), b"same").unwrap();
    assert_eq!(scan(dir.path(), None, None).unwrap().root.bytes, 8);
}

fn shared_for<'a>(
    root: &'a Path,
    root_dev: u64,
    counter: &'a AtomicU64,
    links: &'a Mutex<HashSet<(u64, u64)>>,
) -> Shared<'a> {
    Shared {
        counter,
        cancel: None,
        links,
        root,
        root_dev,
        max_depth: MAX_DEPTH,
    }
}

#[test]
fn vanished_entries_are_ignored_but_other_errors_are_lower_bounds() {
    let dir = temp();
    let (counter, links) = (AtomicU64::new(0), Mutex::new(HashSet::new()));
    let mut walker = Walker::new(shared_for(dir.path(), 0, &counter, &links));
    let gone = dir.path().join("gone");
    assert!(
        matches!(walker.walk(&gone, 0), Visit::Ignored),
        "real missing path"
    );
    let not_found = io::Error::from(io::ErrorKind::NotFound);
    assert!(matches!(
        walker.stat_failed(&gone, &not_found),
        Visit::Ignored
    ));
    assert_eq!((walker.skipped_count, walker.incomplete), (0, false));
    let denied = io::Error::from(io::ErrorKind::PermissionDenied);
    assert!(matches!(walker.stat_failed(&gone, &denied), Visit::Failed));
    assert_eq!((walker.skipped_count, walker.incomplete), (1, true));
}

#[test]
fn volume_rule_skips_the_data_volume_from_any_ancestor() {
    let (root, data) = (Path::new("/"), Path::new(DATA_VOLUME));
    assert!(
        is_other_volume(root, 7, data, 7),
        "same device, still skipped"
    );
    assert!(is_other_volume(root, 7, Path::new("/Volumes/Backup"), 8));
    assert!(!is_other_volume(root, 7, Path::new("/Users"), 7));
    assert!(
        !is_other_volume(data, 7, data, 7),
        "scanning the data volume itself"
    );
    for ancestor in ["/System", "/System/Volumes"] {
        assert!(
            is_other_volume(Path::new(ancestor), 7, data, 7),
            "drill-down into {ancestor}"
        );
    }
    let users = Path::new("/Users");
    assert!(!is_other_volume(users, 7, &users.join("me"), 7));
    assert!(is_other_volume(users, 7, &users.join("mnt"), 9));
}

#[test]
fn other_volume_directories_are_counted_not_entered_or_unreadable() {
    let dir = temp();
    fs::create_dir(dir.path().join("mnt")).unwrap();
    fs::write(dir.path().join("mnt/f"), b"abc").unwrap();
    let real_dev = device(&fs::symlink_metadata(dir.path()).unwrap());
    let (counter, links) = (AtomicU64::new(0), Mutex::new(HashSet::new()));
    // A different root device makes every directory look like a foreign mount.
    let mut walker = Walker::new(shared_for(dir.path(), real_dev ^ 1, &counter, &links));
    assert!(matches!(
        walker.walk(&dir.path().join("mnt"), 0),
        Visit::Ignored
    ));
    assert_eq!(walker.other_volume_count, 1);
    assert_eq!((walker.skipped_count, walker.incomplete), (0, false));
    assert_eq!(counter.load(Ordering::Relaxed), 0);
    let result = scan(dir.path(), None, None).unwrap();
    assert_eq!((result.other_volume_count, result.root.bytes), (0, 3));
}

#[test]
fn a_preset_flag_scans_nothing_and_flags_the_report() {
    let dir = temp();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for i in 0..20 {
        fs::write(sub.join(format!("f{i}")), b"x").unwrap();
    }
    let full = scan(dir.path(), None, None).unwrap();
    assert_eq!(full.files_scanned, 20);
    let flag = AtomicBool::new(true);
    let stopped = scan(dir.path(), Some(&flag), None).unwrap();
    assert!(stopped.cancelled);
    assert_eq!(stopped.files_scanned, 0);
}

#[test]
fn cancelling_while_scanning_stops_the_remaining_branches() {
    let dir = temp();
    for branch in 0..8 {
        let sub = dir.path().join(format!("b{branch}"));
        fs::create_dir(&sub).unwrap();
        for i in 0..5 {
            fs::write(sub.join(format!("f{i}")), b"x").unwrap();
        }
    }
    let flag = AtomicBool::new(false);
    // One thread makes the branch order sequential: the flag flips after the first branch ends.
    let cancel_after_first_branch = |progress: crate::progress::Progress| {
        if progress.completed >= 1 {
            flag.store(true, Ordering::Release);
        }
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let report = pool
        .install(|| scan(dir.path(), Some(&flag), Some(&cancel_after_first_branch)))
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(
        report.files_scanned, 5,
        "only the branch that was running finished"
    );
}

#[test]
fn skipped_paths_are_capped_but_counted_in_full_across_branches() {
    let dir = temp();
    let many = dir.path().join("many");
    fs::create_dir(&many).unwrap();
    let mut locked = Vec::new();
    // One branch alone exceeds the cap, and the top-level ones push the merged total past it.
    for i in 0..MAX_SKIPPED + 5 {
        let path = many.join(format!("l{i}"));
        lock(&path);
        locked.push(path);
    }
    for i in 0..10 {
        let path = dir.path().join(format!("t{i}"));
        lock(&path);
        locked.push(path);
    }
    let _restore = Restore(locked);
    let report = scan(dir.path(), None, None).unwrap();
    assert_eq!(report.skipped_paths.len(), MAX_SKIPPED);
    assert_eq!(report.skipped_count, (MAX_SKIPPED + 5 + 10) as u64);
    assert!(report.incomplete);
}
