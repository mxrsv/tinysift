//! `brew info --cask --json=v2`: what a cask would install, read-only.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::brew::Brew;
use super::elevation::Elevation;
use super::failure::{classify_exit, CaskFailure};

/// `brew info` reads the local index, so it returns in about a second; this
/// only bounds a broken Homebrew.
const INFO_TIMEOUT: Duration = Duration::from_secs(60);
/// Where an `app` artifact lands when the JSON names no `target` (`appdir`).
const DEFAULT_APPDIR: &str = "/Applications";
const MAX_TOKEN_LEN: usize = 100;

/// What `brew info` says about one cask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaskInfo {
    pub token: String,
    /// The cask's first display name, else its token.
    pub name: String,
    /// The version `brew install` would install, as Homebrew spells it
    /// (`4.2.1,20260531.1` keeps its build after the comma).
    pub version: String,
    /// The version Homebrew recorded when it installed the cask, if it did.
    pub installed: Option<String>,
    /// The version of the app on disk when `brew` last read it, which can be
    /// newer than `installed` when the app updated itself.
    pub bundle_version: Option<String>,
    /// Host of the cask's download URL (`brew` may follow a redirect).
    pub download_host: Option<String>,
    /// What the cask asks of `sudo`, which `brew` cannot be given here.
    pub elevation: Elevation,
    /// `brew outdated` (without `--greedy`) would list the cask.
    pub outdated: bool,
    /// Where each `app` artifact is placed.
    pub apps: Vec<PathBuf>,
}

impl CaskInfo {
    /// The version to show as "from": the app on disk when `brew` read it,
    /// else what Homebrew recorded.
    pub fn from_version(&self) -> Option<&str> {
        self.bundle_version.as_deref().or(self.installed.as_deref())
    }
}

/// The version as people write it: Homebrew keeps a build number after a
/// comma (`0.4.26,4`), and so does its own display. For showing only, never
/// for matching or for what `brew` is given.
pub fn display_version(version: &str) -> &str {
    version.split(',').next().unwrap_or(version)
}

/// A bare cask token: lower-case letters, digits and `-_.@+`, starting with a
/// letter or digit. It cannot be a flag (`-x`), a path or a tap-qualified
/// name (`user/tap/cask`), so `brew` can only read it as a core cask.
pub fn valid_token(token: &str) -> bool {
    let mut chars = token.chars();
    token.len() <= MAX_TOKEN_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.@+".contains(c))
}

/// Runs `brew info --cask --json=v2 <tokens>` and reads the result, in the
/// order `brew` printed it. No token reaches `brew` unless every one is valid.
pub fn read_cask_info(brew: &Brew<'_>, tokens: &[&str]) -> Result<Vec<CaskInfo>, CaskFailure> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(bad) = tokens.iter().find(|token| !valid_token(token)) {
        return Err(CaskFailure::NotInCatalogue {
            token: (*bad).to_string(),
        });
    }
    let mut args = vec!["info", "--cask", "--json=v2"];
    args.extend_from_slice(tokens);
    let outcome = brew.output(&args, INFO_TIMEOUT)?;
    if !outcome.success() {
        return Err(classify_exit(tokens[0], &outcome));
    }
    parse_cask_info(&outcome.stdout)
}

#[derive(Deserialize)]
struct Document {
    casks: Vec<RawCask>,
}

#[derive(Deserialize)]
struct RawCask {
    token: String,
    #[serde(default)]
    name: Vec<String>,
    version: Option<String>,
    url: Option<String>,
    installed: Option<String>,
    bundle_short_version: Option<String>,
    #[serde(default)]
    outdated: bool,
    #[serde(default)]
    artifacts: Vec<Value>,
}

pub(super) fn parse_cask_info(json: &str) -> Result<Vec<CaskInfo>, CaskFailure> {
    let unreadable = |detail: String| CaskFailure::BrewFailed {
        exit_code: None,
        detail,
    };
    let document: Document =
        serde_json::from_str(json).map_err(|e| unreadable(format!("unreadable cask info: {e}")))?;
    document
        .casks
        .into_iter()
        .map(|raw| {
            let version = raw
                .version
                .filter(|version| !version.is_empty())
                .ok_or_else(|| unreadable(format!("{} has no version for this Mac", raw.token)))?;
            Ok(CaskInfo {
                name: raw
                    .name
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| raw.token.clone()),
                download_host: raw.url.as_deref().and_then(host_of),
                elevation: Elevation::of(&raw.artifacts),
                apps: app_targets(&raw.artifacts),
                token: raw.token,
                version,
                installed: raw.installed,
                bundle_version: raw.bundle_short_version,
                outdated: raw.outdated,
            })
        })
        .collect()
}

/// The host of an `http(s)` URL, without credentials or port.
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The path of every `app` artifact: its `target` when the JSON has one.
fn app_targets(artifacts: &[Value]) -> Vec<PathBuf> {
    artifacts
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|artifact| {
            let names = artifact.get("app")?.as_array()?;
            match artifact.get("target").and_then(Value::as_str) {
                Some(target) => Some(PathBuf::from(target)),
                None => Some(PathBuf::from(DEFAULT_APPDIR).join(names.first()?.as_str()?)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
