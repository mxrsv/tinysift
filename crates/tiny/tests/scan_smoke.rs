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
