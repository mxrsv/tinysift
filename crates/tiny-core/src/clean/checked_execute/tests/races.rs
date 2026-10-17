//! Filesystem swaps around the validation. FakeTrash only.
//!
//! `execute_checked` validates a path and then hands it to `Trash`; the real
//! Finder mover is path-based, so a swap after validation cannot be detected
//! here. The `residual_race_*` tests record that gap. They are NOT evidence
//! of atomicity. The probe and ancestor tests show what IS caught.

use std::os::unix::fs::symlink;

use super::*;

/// Runs a filesystem mutation, then records the move like `FakeTrash`.
struct SwappingTrash<'a> {
    swap: Box<dyn Fn() + Send + Sync + 'a>,
    inner: FakeTrash,
}

impl Trash for SwappingTrash<'_> {
    fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
        (self.swap)();
        self.inner.move_to_trash(path)
    }
}

/// Runs a filesystem mutation while the app check is running, which is
/// before the path checks.
struct SwappingProbe<'a>(Box<dyn Fn() + Send + Sync + 'a>);

impl AppProbe for SwappingProbe<'_> {
    fn probe(&self, _: &str) -> std::result::Result<bool, String> {
        (self.0)();
        Ok(false)
    }
}

/// Renames `dir` to `<dir>-real` and puts a symlink to it at `dir`.
fn swap_for_symlink(dir: &Path) {
    let real = dir.with_file_name(format!(
        "{}-real",
        dir.file_name().unwrap().to_string_lossy()
    ));
    fs::rename(dir, &real).unwrap();
    symlink(&real, dir).unwrap();
}

/// A different file of a different size at `path`, swapped in atomically.
fn swap_for_other_file(path: &Path) {
    let staged = path.with_extension("staged");
    fs::write(&staged, b"a different, longer file").unwrap();
    fs::rename(&staged, path).unwrap();
}

fn gated() -> Vec<Box<dyn CleanProvider>> {
    let mut provider = fixture("logs");
    provider.app = Some("Xcode");
    vec![Box::new(provider)]
}

#[test]
fn residual_race_a_target_swapped_inside_the_mover_is_still_handed_over() {
    let root = fixture_root("race-target", &["a"]);
    let item = plan("logs", &root, "a");
    let target = root.join("a");
    let trash = SwappingTrash {
        swap: Box::new(|| swap_for_other_file(&target)),
        inner: FakeTrash::default(),
    };
    let report = run(
        &[item],
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        None,
    );
    // Validation already passed, so the mover is called with the planned
    // path and would act on whatever is there now. Not closable here.
    assert_eq!(outcomes(&report), vec![ItemOutcome::MovedToTrash]);
    assert_eq!(*trash.inner.moved.lock().unwrap(), vec![target.clone()]);
    assert_eq!(fs::read(&target).unwrap(), b"a different, longer file");
    let _ = remove_recursive_safe(&root);
}

#[test]
fn residual_race_a_parent_swapped_for_a_symlink_inside_the_mover_is_still_handed_over() {
    let root = fixture_root("race-parent", &[]);
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/f"), b"1234").unwrap();
    let item = plan("logs", &root, "sub/f");
    let parent = root.join("sub");
    let trash = SwappingTrash {
        swap: Box::new(|| swap_for_symlink(&parent)),
        inner: FakeTrash::default(),
    };
    let report = run(
        &[item],
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(outcomes(&report), vec![ItemOutcome::MovedToTrash]);
    assert_eq!(*trash.inner.moved.lock().unwrap(), vec![root.join("sub/f")]);
    // The planned path now resolves through a symlink.
    assert!(fs::symlink_metadata(&parent)
        .unwrap()
        .file_type()
        .is_symlink());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn swaps_during_the_app_probe_are_caught_by_the_path_checks_after_it() {
    // Each case mutates the world during the probe; the path checks run
    // afterwards and must see it.
    type Swap = fn(&Path);
    let cases: [(&str, Swap, SkipReason); 3] = [
        (
            "replaced",
            |r| swap_for_other_file(&r.join("sub/f")),
            SkipReason::Changed,
        ),
        (
            "leaf-symlink",
            |r| {
                fs::remove_file(r.join("sub/f")).unwrap();
                symlink(r.join("sub/f-real"), r.join("sub/f")).unwrap();
            },
            SkipReason::Symlink,
        ),
        (
            "parent-symlink",
            |r| swap_for_symlink(&r.join("sub")),
            SkipReason::OutsideRoots,
        ),
    ];
    for (name, swap, expected) in cases {
        let root = fixture_root(&format!("probe-{name}"), &[]);
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/f"), b"1234").unwrap();
        fs::write(root.join("sub/f-real"), b"1234").unwrap();
        let item = plan("logs", &root, "sub/f");
        let probe = SwappingProbe(Box::new(|| swap(&root)));
        let trash = FakeTrash::default();
        let report = run(&[item], gated(), &probe, &trash, None);
        assert_eq!(
            outcomes(&report),
            vec![ItemOutcome::Skipped(expected)],
            "swap during probe: {name}"
        );
        assert!(
            trash.moved.lock().unwrap().is_empty(),
            "{name} reached Trash"
        );
        let _ = remove_recursive_safe(&root);
    }
}

#[test]
fn a_symlink_at_any_level_from_the_root_down_to_the_item_is_skipped() {
    // root/a/b/c/item; `levels` are the directories plus the item itself.
    let levels = ["", "a", "a/b", "a/b/c", "a/b/c/item"];
    for level in levels {
        let holder = fixture_root(&format!("levels-{}", level.replace('/', "_")), &[]);
        let root = holder.join("root");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/b/c/item"), b"1234").unwrap();
        let item = plan("logs", &root, "a/b/c/item");
        let swapped = if level.is_empty() {
            root.clone()
        } else {
            root.join(level)
        };
        // A symlink to a renamed real copy keeps the item reachable by path.
        swap_for_symlink(&swapped);

        let trash = FakeTrash::default();
        let report = run(
            &[item],
            vec![Box::new(fixture("logs"))],
            &MockChecker::none(),
            &trash,
            None,
        );
        let expected = if level == "a/b/c/item" {
            SkipReason::Symlink
        } else {
            SkipReason::OutsideRoots
        };
        assert_eq!(
            outcomes(&report),
            vec![ItemOutcome::Skipped(expected)],
            "symlink at level {level:?}"
        );
        assert!(
            trash.moved.lock().unwrap().is_empty(),
            "symlink at level {level:?} reached Trash"
        );
        let _ = remove_recursive_safe(&holder);
    }
}
