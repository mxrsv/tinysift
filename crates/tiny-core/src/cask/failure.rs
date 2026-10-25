//! Why a `brew` call or run did not work, as a typed case with a plain problem
//! and recovery. The type is the contract; the sentences are English
//! diagnostics the client may replace with its own copy.

use std::fmt;

use crate::runner::{CommandError, CommandOutcome};

/// Longest `detail` kept from `brew`'s own text.
const DETAIL_LIMIT: usize = 600;
/// Lines of `brew` output kept when it printed no `Error:` line.
const DETAIL_TAIL_LINES: usize = 5;

/// `brew` says one of these when a cask needs `sudo` and may not use it.
const NEEDS_ADMIN: &[&str] = &[
    "requires sudo",
    "sudo is disabled",
    "homebrew_no_sudo",
    "a password is required",
    "terminal is required to read the password",
    "no tty present",
];

/// Curl and resolver messages that mean the network, not the app, failed.
const NETWORK: &[&str] = &[
    "could not resolve host",
    "failed to connect",
    "connection timed out",
    "operation timed out",
    "network is unreachable",
    "connection refused",
    "temporary failure in name resolution",
    "internet connection appears to be offline",
    "curl: (6)",
    "curl: (7)",
    "curl: (28)",
    "curl: (35)",
    "curl: (56)",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaskFailure {
    /// No `brew` on this Mac, or it vanished.
    HomebrewMissing,
    /// An install asked for a token that is not in the bundled catalogue.
    NotInCatalogue {
        token: String,
    },
    /// An update asked for a cask `brew` no longer calls outdated.
    NotOutdated {
        token: String,
    },
    /// The cask's installer needs an administrator password.
    NeedsAdministrator,
    /// The app is open, so replacing it is refused.
    AppRunning {
        app: String,
    },
    /// Whether the app is open could not be told, so replacing it is refused.
    AppCheckFailed {
        app: String,
        error: String,
    },
    /// An app already sits where the cask would put its own.
    AppExists {
        path: String,
    },
    /// Homebrew already lists the cask as installed.
    AlreadyInstalled {
        token: String,
    },
    Network {
        detail: String,
    },
    CaskNotFound {
        token: String,
    },
    /// `brew` was stopped after the longest time a run may take.
    TimedOut,
    /// Any other `brew` failure; `detail` is its own text, shortened.
    BrewFailed {
        exit_code: Option<i32>,
        detail: String,
    },
}

impl CaskFailure {
    /// One plain sentence naming what went wrong.
    pub fn problem(&self) -> String {
        match self {
            Self::HomebrewMissing => "Homebrew is not installed on this Mac.".into(),
            Self::NotInCatalogue { token } => {
                format!("{token} is not an app TinySift can install.")
            }
            Self::NotOutdated { token } => format!("{token} is already up to date."),
            Self::NeedsAdministrator => "This app's installer asks for an administrator password, \
                 which TinySift cannot give Homebrew."
                .into(),
            Self::AppRunning { app } => format!("{app} is open."),
            Self::AppCheckFailed { app, .. } => {
                format!("TinySift could not tell whether {app} is open.")
            }
            Self::AppExists { path } => format!("An app already sits at {path}."),
            Self::AlreadyInstalled { token } => {
                format!("Homebrew already lists {token} as installed.")
            }
            Self::Network { .. } => "Homebrew could not reach the network.".into(),
            Self::CaskNotFound { token } => format!("Homebrew has no app called {token}."),
            Self::TimedOut => "Homebrew took too long and was stopped.".into(),
            Self::BrewFailed { .. } => "Homebrew reported an error.".into(),
        }
    }

    /// One plain sentence saying what to do next.
    pub fn recovery(&self) -> String {
        match self {
            Self::HomebrewMissing => "Install Homebrew from brew.sh, then check again.".into(),
            Self::NotInCatalogue { .. } => "Pick an app from the list.".into(),
            Self::NotOutdated { .. } => "Look for updates again.".into(),
            Self::NeedsAdministrator => {
                "Run the command in Terminal, where Homebrew can ask for the password.".into()
            }
            Self::AppRunning { app } => format!("Quit {app}, then try again."),
            Self::AppCheckFailed { app, .. } => format!("Quit {app} yourself, then try again."),
            Self::AppExists { .. } => {
                "Open the app that is there, or move it away and try again.".into()
            }
            Self::AlreadyInstalled { token } => format!(
                "Open it from Applications, or remove it with `brew uninstall --cask {token}` \
                 and try again."
            ),
            Self::Network { .. } => "Check your connection, then try again.".into(),
            Self::CaskNotFound { .. } => {
                "Update Homebrew's list with `brew update` in Terminal, then try again.".into()
            }
            Self::TimedOut => "Try again on a faster connection.".into(),
            Self::BrewFailed { .. } => {
                "Read Homebrew's message, fix the cause, then try again.".into()
            }
        }
    }

    /// `brew`'s own words or the underlying error, when there are some.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Network { detail } | Self::BrewFailed { detail, .. } => Some(detail),
            Self::AppCheckFailed { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for CaskFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.problem())
    }
}

impl std::error::Error for CaskFailure {}

impl From<CommandError> for CaskFailure {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotFound(_) => Self::HomebrewMissing,
            CommandError::Timeout { .. } => Self::TimedOut,
            CommandError::Spawn { detail, .. } => Self::BrewFailed {
                exit_code: None,
                detail: brief(&detail),
            },
            CommandError::NonUtf8(_) => Self::BrewFailed {
                exit_code: None,
                detail: "Homebrew printed text TinySift could not read.".into(),
            },
        }
    }
}

/// Reads a `brew` that exited unsuccessfully. `token` names the cask the
/// call was about when it was about one.
pub(super) fn classify_exit(token: &str, outcome: &CommandOutcome) -> CaskFailure {
    let text = format!("{}\n{}", outcome.stderr, outcome.stdout);
    let lower = text.to_lowercase();
    let says = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if says(NEEDS_ADMIN) {
        CaskFailure::NeedsAdministrator
    } else if says(&[
        "is unavailable: no cask with this name exists",
        "no available cask",
    ]) {
        CaskFailure::CaskNotFound {
            token: quoted_after(&text, "Cask ").unwrap_or_else(|| token.to_string()),
        }
    } else if let Some(path) = quoted_after(&text, "there is already ") {
        CaskFailure::AppExists { path }
    } else if says(NETWORK) {
        CaskFailure::Network {
            detail: brief(&text),
        }
    } else {
        CaskFailure::BrewFailed {
            exit_code: outcome.status,
            detail: brief(&text),
        }
    }
}

/// The text of the first pair of single quotes after `marker`, which is
/// matched ignoring ASCII case.
fn quoted_after(text: &str, marker: &str) -> Option<String> {
    let start = text
        .to_ascii_lowercase()
        .find(&marker.to_ascii_lowercase())?
        + marker.len();
    let after = &text[start..];
    let inner = &after[after.find('\'')? + 1..];
    Some(inner[..inner.find('\'')?].to_string())
}

/// What the user needs from `brew`'s text: its `Error:` lines, else the last
/// few lines, cut to a length a sheet can show.
fn brief(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let errors: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("Error:"))
        .collect();
    let kept = if errors.is_empty() {
        lines[lines.len().saturating_sub(DETAIL_TAIL_LINES)..].to_vec()
    } else {
        errors
    };
    let joined = kept.join("\n");
    match joined.char_indices().nth(DETAIL_LIMIT) {
        Some((end, _)) => format!("{}…", &joined[..end]),
        None => joined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(status: i32, stderr: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(status),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn a_missing_cask_names_the_token_brew_printed() {
        let failure = classify_exit(
            "wanted",
            &outcome(
                1,
                "Error: Cask 'definitely-not-a-cask' is unavailable: No Cask with this name exists.\n",
            ),
        );
        assert_eq!(
            failure,
            CaskFailure::CaskNotFound {
                token: "definitely-not-a-cask".into()
            }
        );
    }

    #[test]
    fn a_sudo_refusal_is_needs_administrator_whichever_way_brew_says_it() {
        for text in [
            "Error: ghost: The pkg artifact requires sudo, but HOMEBREW_NO_SUDO is set.",
            "sudo: a terminal is required to read the password; either use -S",
            "Error: sudo is disabled by HOMEBREW_NO_SUDO.",
            "sudo: a password is required",
        ] {
            assert_eq!(
                classify_exit("ghost", &outcome(1, text)),
                CaskFailure::NeedsAdministrator,
                "{text}"
            );
        }
    }

    #[test]
    fn network_failures_are_told_from_other_download_errors() {
        let offline = "Error: Download failed on Cask 'x' with message: \
                       curl: (6) Could not resolve host: example.com";
        assert!(matches!(
            classify_exit("x", &outcome(1, offline)),
            CaskFailure::Network { .. }
        ));
        let missing = "Error: Download failed on Cask 'x' with message: \
                       curl: (22) The requested URL returned error: 404";
        assert!(matches!(
            classify_exit("x", &outcome(1, missing)),
            CaskFailure::BrewFailed { .. }
        ));
    }

    #[test]
    fn an_app_in_the_way_is_named_by_its_path() {
        let failure = classify_exit(
            "chatgpt",
            &outcome(
                1,
                "Error: It seems there is already an App at '/Applications/ChatGPT.app'.",
            ),
        );
        assert_eq!(
            failure,
            CaskFailure::AppExists {
                path: "/Applications/ChatGPT.app".into()
            }
        );
    }

    #[test]
    fn other_failures_keep_brews_error_lines_and_the_exit_code() {
        let noisy = format!(
            "{}\nError: Failure while executing; `/usr/bin/xattr` exited with 1.\nError: second",
            "progress ".repeat(50)
        );
        assert_eq!(
            classify_exit("x", &outcome(3, &noisy)),
            CaskFailure::BrewFailed {
                exit_code: Some(3),
                detail: "Error: Failure while executing; `/usr/bin/xattr` exited with 1.\n\
                         Error: second"
                    .into()
            }
        );
    }

    #[test]
    fn text_without_an_error_line_keeps_its_tail_cut_at_a_character() {
        let text = (0..20).map(|n| format!("line {n}\n")).collect::<String>();
        let detail = brief(&text);
        assert_eq!(detail, "line 15\nline 16\nline 17\nline 18\nline 19");
        let long = "é".repeat(DETAIL_LIMIT + 10);
        let cut = brief(&long);
        assert_eq!(
            cut.chars().count(),
            DETAIL_LIMIT + 1,
            "text plus an ellipsis"
        );
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn runner_errors_map_to_typed_failures() {
        assert_eq!(
            CaskFailure::from(CommandError::NotFound("brew".into())),
            CaskFailure::HomebrewMissing
        );
        assert_eq!(
            CaskFailure::from(CommandError::Timeout {
                bin: "brew".into(),
                timeout_ms: 1
            }),
            CaskFailure::TimedOut
        );
        assert!(matches!(
            CaskFailure::from(CommandError::Spawn {
                bin: "brew".into(),
                detail: "denied".into()
            }),
            CaskFailure::BrewFailed {
                exit_code: None,
                ..
            }
        ));
    }

    #[test]
    fn every_failure_has_a_problem_and_a_recovery() {
        let all = [
            CaskFailure::HomebrewMissing,
            CaskFailure::NotInCatalogue { token: "t".into() },
            CaskFailure::NotOutdated { token: "t".into() },
            CaskFailure::NeedsAdministrator,
            CaskFailure::AppRunning { app: "A".into() },
            CaskFailure::AppCheckFailed {
                app: "A".into(),
                error: "e".into(),
            },
            CaskFailure::AppExists { path: "/p".into() },
            CaskFailure::AlreadyInstalled { token: "t".into() },
            CaskFailure::Network { detail: "d".into() },
            CaskFailure::CaskNotFound { token: "t".into() },
            CaskFailure::TimedOut,
            CaskFailure::BrewFailed {
                exit_code: Some(1),
                detail: "d".into(),
            },
        ];
        for failure in all {
            assert!(failure.problem().ends_with('.'), "{failure:?}");
            assert!(failure.recovery().ends_with('.'), "{failure:?}");
            assert_eq!(failure.to_string(), failure.problem());
        }
    }
}
