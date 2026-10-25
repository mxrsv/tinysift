//! The casks Homebrew has a record of, whether or not their apps are still
//! where they were put.

use std::time::Duration;

use serde::Deserialize;

use super::brew::Brew;
use super::failure::{classify_exit, CaskFailure};

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

/// Runs `brew list --cask --versions --json`. The plain form of the command
/// exits 1 on a Mac that has a cask from an untrusted tap, which would hide
/// every other cask; the JSON form does not.
pub fn list_tracked(brew: &Brew<'_>) -> Result<Vec<TrackedCask>, CaskFailure> {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::cask::test_support::*;
    use crate::runner::test_support::MockRunner;

    const ARGS: [&str; 4] = ["list", "--cask", "--versions", "--json"];

    fn tracked(runner: &MockRunner) -> Result<Vec<TrackedCask>, CaskFailure> {
        list_tracked(&Brew::new(Path::new(BREW_PATH), runner))
    }

    #[test]
    fn the_json_listing_is_read_and_the_plain_one_is_never_asked_for() {
        let json = r#"{"formulae":[],"casks":[
            {"token":"alt-tab","versions":["7.24.0"],"pinned_version":null},
            {"token":"two","versions":["1","2"],"pinned_version":null},
            {"token":"bare"}]}"#;
        let runner = MockRunner::new().with_exit(BREW_PATH, &ARGS, 0, json);
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
            [format!("{BREW_PATH} list --cask --versions --json")]
        );
    }

    #[test]
    fn a_failing_or_garbled_list_is_a_typed_failure() {
        let failed = MockRunner::new().with_exit(BREW_PATH, &ARGS, 1, "");
        assert!(matches!(
            tracked(&failed),
            Err(CaskFailure::BrewFailed {
                exit_code: Some(1),
                ..
            })
        ));
        let garbled = MockRunner::new().with_exit(BREW_PATH, &ARGS, 0, "jq: not found");
        assert!(matches!(
            tracked(&garbled),
            Err(CaskFailure::BrewFailed {
                exit_code: None,
                ..
            })
        ));
    }
}
