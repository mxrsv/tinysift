//! Deletion as administrator, tested without ever asking for it: the path
//! checks run on their own, and the constant script runs through the real
//! `osascript` only without its administrator clause, on temp folders the
//! tests create. `OsascriptDelete` is never built here.

use super::*;
use crate::clean::providers::system_simulator_caches::{
    check_entry, CACHES_ROOT, SIMULATOR_ACTIVITY,
};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// The script's only administrator clause, which the test variant drops.
const ADMIN_CLAUSE: &str = r#" with prompt "TinySift wants to delete the system simulator caches." with administrator privileges"#;

/// The processes the root shell guards on, as the script names them.
const ACTIVITY: &str = "Xcode Simulator xcodebuild xctest XCTRunner simctl";

/// A physical temp folder holding `Caches/` (with `dyld/23A8464/cache`,
/// `index.db`, `sibling` and a symlink `link`) and `outside/keep`.
fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (root, outside) = (base.join("Caches"), base.join("outside"));
    fs::create_dir_all(root.join("dyld/23A8464")).unwrap();
    fs::write(root.join("dyld/23A8464/cache"), b"cache").unwrap();
    fs::write(root.join("index.db"), b"db").unwrap();
    fs::write(root.join("sibling"), b"stays").unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    (dir, root, outside)
}

#[test]
fn the_script_guards_every_simulator_tool_and_runs_only_apple_binaries() {
    assert_eq!(ACTIVITY, SIMULATOR_ACTIVITY.join(" "));
    for fragment in [ADMIN_CLAUSE, ACTIVITY, CACHES_ROOT] {
        assert_eq!(APPLESCRIPT.matches(fragment).count(), 1, "{fragment}");
    }
    for service in ["simdiskimaged", "CoreSimulatorService", "SimLaunchHost"] {
        assert!(
            !APPLESCRIPT.contains(service),
            "{service} runs without Xcode"
        );
    }
    let absolute: BTreeSet<&str> = APPLESCRIPT
        .split([' ', '"', '='])
        .filter(|word| word.starts_with('/'))
        .map(|word| word.trim_end_matches(';'))
        .collect();
    assert_eq!(
        absolute,
        BTreeSet::from([CACHES_ROOT, "/bin/rm", "/bin/sh", "/usr/bin/pgrep"])
    );
    assert!(APPLESCRIPT.contains(r#"/bin/rm -rfx -- \"./$name\""#));
    assert!(APPLESCRIPT.contains(r#"[ -L \"./$name\" ]"#));
}

#[test]
fn only_bare_entries_directly_inside_the_allowed_folder_are_passed() {
    let (_dir, root, _) = layout();
    let check = |path: &Path| check_entry(&root, path);
    let (dyld, db) = (root.join("dyld"), root.join("index.db"));
    assert_eq!(
        entry_names(&[&dyld, &db], check),
        Ok(vec!["dyld", "index.db"])
    );
    let refused = [
        root.join("dyld/23A8464"),
        root.clone(),
        root.join("link"),
        root.join("missing"),
        root.join("../Caches/dyld"),
        root.join("dyld/.."),
        PathBuf::from("Caches/dyld"),
        PathBuf::from("/System/Library/Caches/dyld"),
        root.join(OsStr::from_bytes(b"caf\xe9")),
        root.join("dyld\0"),
    ];
    for path in &refused {
        let names = entry_names(&[&dyld, path], check);
        assert!(names.is_err(), "{} refuses the whole batch", path.display());
    }
    // A symlinked folder above the entry leads somewhere else.
    let alias = root.parent().unwrap().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let through_alias = alias.join("dyld");
    assert!(entry_names(&[&through_alias], |path| check_entry(&alias, path)).is_err());
}

#[test]
fn the_real_route_checks_against_the_fixed_caches_folder() {
    // What `OsascriptDelete` runs. None of these can be an entry of
    // `CACHES_ROOT`, so the checks refuse them before reading the disk there.
    let (_dir, root, _) = layout();
    let paths = [
        root.join("dyld"),
        PathBuf::from(CACHES_ROOT),
        Path::new(CACHES_ROOT).join("dyld/23A8464"),
        Path::new(CACHES_ROOT).join("../Images"),
        Path::new(CACHES_ROOT).join("../../../../System/Library"),
        Path::new(CACHES_ROOT).join("dyld/.."),
        Path::new(CACHES_ROOT).join(OsStr::from_bytes(b"caf\xe9")),
        Path::new(CACHES_ROOT).join("tiny-test-missing"),
        PathBuf::from("/System/Library/AssetsV2/x.dmg"),
        PathBuf::from("/System/Volumes/Data/Library/Developer/CoreSimulator/Caches/dyld"),
    ];
    for path in &paths {
        assert!(
            entry_names(&[path.as_path()], check_system_cache_path).is_err(),
            "{}",
            path.display()
        );
    }
}

#[cfg(target_os = "macos")]
mod with_osascript {
    use super::*;

    /// No process has this name, so the guard lets the script go on.
    const NOTHING_RUNS: &str = "tinysift-test-no-such-process";

    /// The production script with only these changes, each found exactly
    /// once: no administrator clause (no dialog; it runs as the test user),
    /// `root` for `CACHES_ROOT` and `activity` for the guarded processes.
    fn variant(root: &Path, activity: &str) -> String {
        let root = root.to_str().unwrap();
        assert!(!root.contains([' ', '"', '\\', '\'']), "{root}");
        let mut script = APPLESCRIPT.to_string();
        for (from, to) in [
            (ADMIN_CLAUSE, ""),
            (CACHES_ROOT, root),
            (ACTIVITY, activity),
        ] {
            assert_eq!(script.matches(from).count(), 1, "{from}");
            script = script.replace(from, to);
        }
        assert!(!script.contains("administrator"));
        script
    }

    fn gone(path: &Path) -> bool {
        fs::symlink_metadata(path).is_err()
    }

    #[test]
    fn the_constant_script_deletes_exactly_the_named_entries() {
        let (_dir, root, outside) = layout();
        // A leading `-e` first: osascript stops reading options at `--` only.
        let names = [
            "-e",
            "-rf",
            "it's",
            "two words",
            "line\nbreak",
            "*",
            "$(touch injected); touch injected",
        ];
        fs::create_dir_all(root.join("it's/inner")).unwrap();
        // Deleting a folder removes a symlink inside it, never its target.
        std::os::unix::fs::symlink(&outside, root.join("it's/inner/escape")).unwrap();
        fs::create_dir(root.join("line\nbreak")).unwrap();
        fs::write(root.join("line\nbreak/file"), b"x").unwrap();
        for file in ["-e", "-rf", "two words", "*", names[6]] {
            fs::write(root.join(file), b"x").unwrap();
        }
        assert_eq!(run_script(&variant(&root, NOTHING_RUNS), &names), Ok(()));
        for name in names {
            assert!(gone(&root.join(name)), "{name:?} is still there");
        }
        for kept in ["sibling", "index.db", "dyld/23A8464/cache", "link"] {
            assert!(!gone(&root.join(kept)), "{kept} was deleted");
        }
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
        assert!(gone(&root.join("injected")) && gone(&root.parent().unwrap().join("injected")));
    }

    #[test]
    fn the_root_shell_refuses_what_validation_would() {
        let (_dir, root, outside) = layout();
        fs::create_dir_all(root.join("nested/inner")).unwrap();
        let refused = run_script(
            &variant(&root, NOTHING_RUNS),
            &["..", ".", "", "nested/inner", "link", "sibling"],
        );
        assert!(
            matches!(&refused, Err(ElevatedError::Failed(detail)) if detail.contains("refused")),
            "{refused:?}"
        );
        assert!(gone(&root.join("sibling")), "valid names still go");
        for kept in ["nested/inner", "link", "index.db", "dyld"] {
            assert!(!gone(&root.join(kept)), "{kept} was deleted");
        }
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");

        // The folder reached through a symlink is not the folder named.
        let alias = root.parent().unwrap().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let moved = run_script(&variant(&alias, NOTHING_RUNS), &["index.db"]);
        assert!(matches!(moved, Err(ElevatedError::Failed(_))), "{moved:?}");
        assert!(!gone(&root.join("index.db")));
    }

    #[test]
    fn a_running_simulator_tool_refuses_the_whole_batch() {
        let (_dir, root, _) = layout();
        // `osascript` runs while its root shell checks, as Simulator might.
        let busy = run_script(&variant(&root, "osascript"), &["sibling", "dyld"]);
        assert_eq!(busy, Err(ElevatedError::Busy("osascript".into())));
        assert!(!gone(&root.join("sibling")) && !gone(&root.join("dyld")));
    }

    #[test]
    fn the_answer_tells_a_cancelled_dialog_from_a_busy_or_failed_run() {
        assert_eq!(answer("0\n"), Ok(()));
        assert_eq!(
            answer("-128\nUser canceled.\n"),
            Err(ElevatedError::Cancelled)
        );
        assert_eq!(
            answer("75\nSimulator\n\n"),
            Err(ElevatedError::Busy("Simulator".into()))
        );
        assert_eq!(
            answer("1\nrm: ./dyld: Operation not permitted\n"),
            Err(ElevatedError::Failed(
                "rm: ./dyld: Operation not permitted (1)".into()
            ))
        );
        assert!(matches!(answer(""), Err(ElevatedError::Failed(_))));
    }
}
