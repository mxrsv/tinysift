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

/// `brew` says one of these when what it downloaded is not what its data names.
const CHECKSUM: &[&str] = &["reports different checksum", "checksum mismatch"];

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
    /// The cask only offers an installer to open by hand, which installs
    /// nothing through `brew`.
    ManualInstaller {
        token: String,
    },
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
    /// The download does not match the checksum Homebrew's data holds, which
    /// is what an index older than the app's newest release looks like.
    StaleData,
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
            Self::ManualInstaller { token } => {
                format!("{token} only comes with an installer to open by hand.")
            }
            Self::AppRunning { app } => format!("{app} is open."),
            Self::AppCheckFailed { app, .. } => {
                format!("TinySift could not tell whether {app} is open.")
            }
            Self::AppExists { path } => format!("An app already sits at {path}."),
            Self::AlreadyInstalled { token } => {
                format!("Homebrew already lists {token} as installed.")
            }
            Self::Network { .. } => "Homebrew could not reach the network.".into(),
            Self::StaleData => "Homebrew's data may be out of date: the download does not match \
                 the checksum it holds."
                .into(),
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
            Self::ManualInstaller { .. } => {
                "Run the command in Terminal to see where Homebrew keeps the installer.".into()
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
            Self::StaleData => {
                "Refresh Homebrew's data with `brew update` in Terminal, then try again.".into()
            }
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
    } else if says(CHECKSUM) {
        CaskFailure::StaleData
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
mod tests;
