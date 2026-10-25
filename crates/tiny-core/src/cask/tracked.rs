//! The casks Homebrew has a record of, whether or not their apps are still
//! where they were put.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::brew::Brew;
use super::failure::{classify_exit, CaskFailure};
use super::info::valid_token;

/// `brew list` reads the Caskroom and returns at once; this only bounds a
/// broken Homebrew.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// A cask in Homebrew's Caskroom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedCask {
    pub token: String,
    /// The versions Homebrew recorded, which are not always the app's own
    /// (an app that updates itself leaves its first one here).
    pub versions: Vec<String>,
}

#[derive(Deserialize)]
struct Document {
    casks: Vec<RawTracked>,
}

#[derive(Deserialize)]
struct RawTracked {
    token: String,
    #[serde(default)]
    versions: Vec<String>,
}

/// Homebrew's record of installed casks. Asks `brew list --cask --versions
/// --json` (the plain form of the command exits 1 on a Mac that has a cask from
/// an untrusted tap, which would hide every other cask); that command needs
/// `jq`, which macOS ships only from version 15. When `brew` cannot answer it
/// the Caskroom folders are read instead, which is where the answer comes from.
pub fn list_tracked(brew: &Brew<'_>) -> Result<Vec<TrackedCask>, CaskFailure> {
    match list_with_brew(brew) {
        Err(failure @ CaskFailure::BrewFailed { .. }) => read_caskroom(brew.path()).ok_or(failure),
        result => result,
    }
}

fn list_with_brew(brew: &Brew<'_>) -> Result<Vec<TrackedCask>, CaskFailure> {
    let outcome = brew.output(&["list", "--cask", "--versions", "--json"], LIST_TIMEOUT)?;
    if !outcome.success() {
        return Err(classify_exit("list", &outcome));
    }
    let document: Document =
        serde_json::from_str(&outcome.stdout).map_err(|e| CaskFailure::BrewFailed {
            exit_code: None,
            detail: format!("unreadable cask list: {e}"),
        })?;
    Ok(document
        .casks
        .into_iter()
        .map(|cask| TrackedCask {
            token: cask.token,
            versions: cask.versions,
        })
        .collect())
}

/// The casks in `<prefix>/Caskroom`, where `<prefix>` holds `bin/brew`: one
/// folder per cask with a folder per version inside. Symlinks and files are
/// skipped, as are names that are not cask tokens and hidden folders
/// (`.metadata`). `None` when the folder cannot be read.
fn read_caskroom(brew: &Path) -> Option<Vec<TrackedCask>> {
    let caskroom = brew.parent()?.parent()?.join("Caskroom");
    let mut casks: Vec<TrackedCask> = real_folders(&caskroom)?
        .into_iter()
        .filter(|(token, _)| valid_token(token))
        .map(|(token, path)| TrackedCask {
            versions: real_folders(&path)
                .unwrap_or_default()
                .into_iter()
                .map(|(version, _)| version)
                .filter(|version| !version.starts_with('.'))
                .collect(),
            token,
        })
        .collect();
    casks.sort_by(|a, b| a.token.cmp(&b.token));
    Some(casks)
}

/// The folders directly inside `dir` as `(name, path)`, sorted by name. The
/// type is read without following a link.
fn real_folders(dir: &Path) -> Option<Vec<(String, std::path::PathBuf)>> {
    let mut folders: Vec<_> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .collect();
    folders.sort();
    Some(folders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;
    use crate::runner::CommandError;

    const ARGS: [&str; 4] = ["list", "--cask", "--versions", "--json"];
    /// A `brew` whose prefix does not exist, so no test reads a real Caskroom.
    const NOWHERE: &str = "/nonexistent/prefix/bin/brew";

    fn tracked(runner: &MockRunner) -> Result<Vec<TrackedCask>, CaskFailure> {
        list_tracked(&Brew::new(Path::new(NOWHERE), runner))
    }

    #[test]
    fn the_json_listing_is_read_and_the_plain_one_is_never_asked_for() {
        let json = r#"{"formulae":[],"casks":[
            {"token":"alt-tab","versions":["7.24.0"],"pinned_version":null},
            {"token":"two","versions":["1","2"],"pinned_version":null},
            {"token":"bare"}]}"#;
        let runner = MockRunner::new().with_exit(NOWHERE, &ARGS, 0, json);
        let casks = tracked(&runner).unwrap();
        assert_eq!(
            casks,
            [
                TrackedCask {
                    token: "alt-tab".into(),
                    versions: vec!["7.24.0".into()]
                },
                TrackedCask {
                    token: "two".into(),
                    versions: vec!["1".into(), "2".into()]
                },
                TrackedCask {
                    token: "bare".into(),
                    versions: Vec::new()
                },
            ]
        );
        assert_eq!(
            *runner.output_calls.lock().unwrap(),
            [format!("{NOWHERE} list --cask --versions --json")]
        );
    }

    #[test]
    fn a_failing_or_garbled_list_with_no_caskroom_to_read_is_a_typed_failure() {
        let failed = MockRunner::new().with_exit(NOWHERE, &ARGS, 1, "");
        assert!(matches!(
            tracked(&failed),
            Err(CaskFailure::BrewFailed {
                exit_code: Some(1),
                ..
            })
        ));
        let garbled = MockRunner::new().with_exit(NOWHERE, &ARGS, 0, "jq: not found");
        assert!(matches!(
            tracked(&garbled),
            Err(CaskFailure::BrewFailed {
                exit_code: None,
                ..
            })
        ));
    }

    fn caskroom_prefix() -> tempfile::TempDir {
        let prefix = tempfile::tempdir().unwrap();
        let caskroom = prefix.path().join("Caskroom");
        for dir in [
            "ghostty/1.3.1",
            "ghostty/.metadata",
            "iterm2/3.5.14",
            "iterm2/3.7.6",
            "Not A Token/1",
        ] {
            fs::create_dir_all(caskroom.join(dir)).unwrap();
        }
        fs::write(caskroom.join("a-file"), "x").unwrap();
        let elsewhere = prefix.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, caskroom.join("linked")).unwrap();
        prefix
    }

    #[test]
    fn without_jq_the_caskroom_answers_and_skips_links_files_and_hidden_folders() {
        let prefix = caskroom_prefix();
        let brew = prefix.path().join("bin/brew");
        let key = brew.to_str().unwrap();
        for failing in [
            MockRunner::new().with_exit(key, &ARGS, 1, ""),
            MockRunner::new().with_exit(key, &ARGS, 0, "not json"),
        ] {
            let casks = list_tracked(&Brew::new(&brew, &failing)).unwrap();
            let seen: Vec<_> = casks
                .iter()
                .map(|c| {
                    (
                        c.token.as_str(),
                        c.versions.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                })
                .collect();
            assert_eq!(
                seen,
                [
                    ("ghostty", vec!["1.3.1"]),
                    ("iterm2", vec!["3.5.14", "3.7.6"])
                ]
            );
        }
    }

    #[test]
    fn a_failure_that_is_not_brews_is_not_papered_over_by_the_caskroom() {
        let prefix = caskroom_prefix();
        let brew = prefix.path().join("bin/brew");
        let timed_out = MockRunner::new().with_output(
            brew.to_str().unwrap(),
            &ARGS,
            Err(CommandError::Timeout {
                bin: "brew".into(),
                timeout_ms: 1,
            }),
        );
        assert_eq!(
            list_tracked(&Brew::new(&brew, &timed_out)),
            Err(CaskFailure::TimedOut { running: false })
        );
    }
}
