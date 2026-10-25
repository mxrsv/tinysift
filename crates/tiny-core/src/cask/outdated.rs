//! The apps Homebrew tracks that have a newer version.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::brew::Brew;
use super::failure::{classify_exit, CaskFailure};
use super::info::{display_version, read_cask_info_lenient, valid_token};
use super::plan::{blocked_by, CaskAction};
use super::version::offers_update;

/// `brew outdated` reads the local index; this only bounds a broken Homebrew.
const OUTDATED_TIMEOUT: Duration = Duration::from_secs(60);

/// A tracked cask with a newer version, ready for the Updates list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutdatedCask {
    pub token: String,
    pub name: String,
    /// The version on the Mac: the app on disk when `brew` last read it, else
    /// what Homebrew recorded.
    pub from_version: String,
    pub version: String,
    pub download_host: Option<String>,
    /// Why updating it would be refused, known from `brew info`.
    pub blocked: Option<CaskFailure>,
    pub apps: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Document {
    casks: Vec<RawOutdated>,
}

#[derive(Deserialize)]
struct RawOutdated {
    /// The cask's token (`brew` calls it a name here).
    name: String,
    #[serde(default)]
    installed_versions: Vec<String>,
    current_version: String,
    #[serde(default)]
    pinned: bool,
}

/// Runs `brew outdated --cask --json=v2`: no `--greedy`, so a self-updating
/// app is listed only when the copy on disk is older than the cask, as
/// Homebrew 7 decides it (`Cask#auto_updates_bundle_outdated?`). That is the
/// whole point: `brew upgrade --cask <token>` always acts greedily and would
/// reinstall an app that already updated itself, so the default list is the
/// only place that says an update is needed. Pinned casks and names that are
/// not bare tokens are left out. A cask from a third-party tap is listed by its
/// bare name when it has no core namesake (`tinycast` is), and is kept: `brew`
/// resolves the installed cask.
fn read_outdated(brew: &Brew<'_>) -> Result<Vec<RawOutdated>, CaskFailure> {
    let outcome = brew.output(&["outdated", "--cask", "--json=v2"], OUTDATED_TIMEOUT)?;
    if !outcome.success() {
        return Err(classify_exit("outdated", &outcome));
    }
    let document: Document =
        serde_json::from_str(&outcome.stdout).map_err(|e| CaskFailure::BrewFailed {
            exit_code: None,
            detail: format!("unreadable outdated list: {e}"),
        })?;
    Ok(document
        .casks
        .into_iter()
        .filter(|cask| !cask.pinned && valid_token(&cask.name))
        .collect())
}

/// Whether `token` is in the default `brew outdated` right now.
pub fn still_outdated(brew: &Brew<'_>, token: &str) -> Result<bool, CaskFailure> {
    Ok(read_outdated(brew)?.iter().any(|cask| cask.name == token))
}

/// The default `brew outdated` and then one `brew info` for the tokens it
/// names. Left out besides what [`read_outdated`] drops: casks with no `app`
/// artifact, such as command-line tools and package-only installers, because
/// this is the Apps screen, casks whose app already is the listed version or
/// newer than it, and casks `brew info` refuses (see [`read_cask_info_lenient`]).
pub fn list_outdated(brew: &Brew<'_>) -> Result<Vec<OutdatedCask>, CaskFailure> {
    let listed = read_outdated(brew)?;
    let tokens: Vec<&str> = listed.iter().map(|cask| cask.name.as_str()).collect();
    let infos = read_cask_info_lenient(brew, &tokens)?;
    Ok(listed
        .iter()
        .filter_map(|cask| {
            let info = infos.iter().find(|info| info.token == cask.name)?;
            if info.apps.is_empty() {
                return None;
            }
            let from_version = info
                .from_version()
                .or_else(|| cask.installed_versions.last().map(String::as_str))?;
            let version = cask.current_version.as_str();
            // Homebrew's record can be older than an app that updated itself
            // (a cask that does not declare `auto_updates`): then `brew
            // outdated` lists the cask while the app on disk already is its
            // version, or newer. Neither is an update; see `version`. The
            // raw texts are compared, so a build-number-only update
            // (`2.0,100` to `2.0,200`) stays.
            if !offers_update(from_version, version) {
                return None;
            }
            Some(OutdatedCask {
                token: info.token.clone(),
                name: info.name.clone(),
                from_version: display_version(from_version).to_string(),
                version: display_version(version).to_string(),
                download_host: info.download_host.clone(),
                blocked: blocked_by(info.elevation, CaskAction::Update, &info.token),
                apps: info.apps.clone(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests;
