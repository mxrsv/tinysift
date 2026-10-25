use assert_cmd::Command;

#[test]
fn scan_json_preserves_cli_schema_and_never_follows_symlinks() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(&root).unwrap();
    let outside = tempfile::tempdir_in(&root).unwrap();
    std::fs::write(fixture.path().join("keep.txt"), b"abc").unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"private").unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.path().join("linked")).unwrap();
    let output = Command::new(assert_cmd::cargo::cargo_bin!("tiny"))
        .args([
            "scan",
            "--json",
            "--min-size-mb",
            "0",
            "--older-than-days",
            "0",
            "--by-ext",
            "--path",
        ])
        .arg(fixture.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let data: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(data["totals"]["files_scanned"], 1);
    assert_eq!(data["large_files"][0]["size"], 3);
    assert_eq!(data["by_extension"][0]["ext"], "txt");
    assert!(outside.path().join("secret.txt").exists());
}

fn scan_json(paths: &[&std::path::Path], without_home: bool) -> serde_json::Value {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("tiny"));
    command.args([
        "scan",
        "--json",
        "--min-size-mb",
        "0",
        "--duplicates",
        "--hash",
    ]);
    if without_home {
        command.env_remove("HOME");
    }
    for path in paths {
        command.arg("--path").arg(path);
    }
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn overlapping_and_repeated_roots_count_each_path_once() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(root).unwrap();
    let child = fixture.path().join("nested");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("same.txt"), b"one real file").unwrap();
    let dotted = fixture.path().join(".");
    let report = scan_json(&[fixture.path(), &child, &dotted], false);
    assert_eq!(report["totals"]["files_scanned"], 1);
    assert_eq!(report["duplicates"], serde_json::json!([]));
    assert_eq!(report["duplicate_total_wasted_bytes"], 0);
}

#[test]
fn explicit_root_works_without_home() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(root).unwrap();
    std::fs::write(fixture.path().join("a.txt"), b"x").unwrap();
    assert_eq!(
        scan_json(&[fixture.path()], true)["totals"]["files_scanned"],
        1
    );
}

#[test]
fn invalid_requested_roots_fail_without_a_success_payload() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(root).unwrap();
    let file = fixture.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let link = fixture.path().join("link");
    std::os::unix::fs::symlink(fixture.path(), &link).unwrap();
    for path in [
        fixture.path().join("missing"),
        file,
        link,
        fixture.path().join("../"),
    ] {
        Command::new(assert_cmd::cargo::cargo_bin!("tiny"))
            .args(["scan", "--json", "--path"])
            .arg(path)
            .assert()
            .failure()
            .stdout(predicates::str::is_empty());
    }
}

#[test]
fn explicit_nested_build_root_remains_scannable() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(root).unwrap();
    let child = fixture.path().join("build");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("a.txt"), b"x").unwrap();
    assert_eq!(
        scan_json(&[fixture.path(), &child], false)["totals"]["files_scanned"],
        1
    );
}

#[test]
fn focus_zero_minutes_is_an_input_error() {
    Command::new(assert_cmd::cargo::cargo_bin!("tiny"))
        .args(["focus", "--minutes", "0"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("minutes must be > 0"));
}

#[test]
fn unreadable_subtree_fails_without_reporting_a_complete_scan() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(root).unwrap();
    let locked = fixture.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("hidden.txt"), b"unreadable").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Privileged runners cannot reproduce Unix permission denial.
    if std::fs::read_dir(&locked).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let outputs: Vec<_> = [fixture.path(), locked.as_path()]
        .iter()
        .map(|path| {
            Command::new(assert_cmd::cargo::cargo_bin!("tiny"))
                .args(["scan", "--json", "--path"])
                .arg(path)
                .output()
        })
        .collect();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    for output in outputs {
        let output = output.unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("locked"));
    }
}

#[test]
fn distinct_hardlink_paths_are_kept() {
    let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let fixture = tempfile::tempdir_in(root).unwrap();
    let first = fixture.path().join("first.txt");
    std::fs::write(&first, b"same inode").unwrap();
    std::fs::hard_link(&first, fixture.path().join("second.txt")).unwrap();
    assert_eq!(
        scan_json(&[fixture.path(), fixture.path()], false)["totals"]["files_scanned"],
        2
    );
}
