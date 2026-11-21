//! The executable is `tinysift`; `sift` is the same file under a second name.
//! Usage and error hints must print the name that was typed, and neither may
//! print the retired name `tiny`.

use std::os::unix::fs::symlink;
use std::process::Command;

use tempfile::TempDir;

const FULL_NAME: &str = "tinysift";
const SHORT_NAME: &str = "sift";

/// A temp dir holding the built executable under both names.
fn both_names() -> (TempDir, [std::path::PathBuf; 2]) {
    let dir = tempfile::tempdir().unwrap();
    let built = assert_cmd::cargo::cargo_bin(FULL_NAME);
    let full = dir.path().join(FULL_NAME);
    let short = dir.path().join(SHORT_NAME);
    symlink(&built, &full).unwrap();
    symlink(&built, &short).unwrap();
    (dir, [full, short])
}

fn text(command: &mut Command) -> String {
    let output = command.output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn usage_prints_the_name_that_was_typed() {
    let (_dir, paths) = both_names();
    for (path, name) in paths.iter().zip([FULL_NAME, SHORT_NAME]) {
        for args in [
            vec!["--help"],
            vec!["clean", "--help"],
            vec!["clean", "--no-such-flag"],
        ] {
            let out = text(Command::new(path).args(&args));
            assert!(
                out.contains(&format!("Usage: {name} ")),
                "{name} {args:?} printed:\n{out}"
            );
            assert!(
                !out.contains("Usage: tiny "),
                "{name} {args:?} still prints the retired name:\n{out}"
            );
        }
    }
}

#[test]
fn both_names_report_the_same_version() {
    let (_dir, [full, short]) = both_names();
    let full = text(Command::new(full).arg("--version"));
    let short = text(Command::new(short).arg("--version"));
    assert_eq!(full, short);
    assert!(full.starts_with(FULL_NAME), "printed: {full}");
}
