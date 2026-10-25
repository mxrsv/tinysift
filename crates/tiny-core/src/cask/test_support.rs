//! Fixtures for the cask tests: `brew info` JSON built to the shape Homebrew
//! 7 prints, and a fake `brew` path. No test runs a real `brew`.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::plan::{CaskAction, CaskPlanItem};
use crate::runner::CommandOutcome;

pub(super) const BREW_PATH: &str = "/opt/homebrew/bin/brew";

/// One cask as `brew info --cask --json=v2` prints it, trimmed to the fields
/// core reads. `artifacts` is the JSON array of its artifacts.
pub(super) fn cask(token: &str, version: &str, artifacts: Value) -> Value {
    json!({
        "token": token,
        "name": [token],
        "url": format!("https://downloads.example.com/{token}/{version}.dmg"),
        "version": version,
        "installed": null,
        "bundle_short_version": null,
        "outdated": false,
        "artifacts": artifacts,
    })
}

/// The `{"app": [...], "target": ...}` artifact of an app in `/Applications`.
pub(super) fn app_artifact(app: &str) -> Value {
    json!({"app": [app], "target": format!("/Applications/{app}")})
}

/// A cask that installs one app and nothing needing `sudo`.
pub(super) fn simple_cask(token: &str, version: &str, app: &str) -> Value {
    cask(
        token,
        version,
        json!([
            {"uninstall": [{"quit": format!("com.example.{token}")}]},
            app_artifact(app),
            {"zap": [{"trash": [format!("~/Library/Caches/com.example.{token}")]}]},
        ]),
    )
}

pub(super) fn info_json(casks: &[Value]) -> String {
    json!({"formulae": [], "casks": casks}).to_string()
}

/// A plan item for `token` whose app would be placed at `app`.
pub(super) fn plan_item(action: CaskAction, token: &str, app: PathBuf) -> CaskPlanItem {
    CaskPlanItem {
        action,
        token: token.into(),
        name: token.into(),
        from_version: (action == CaskAction::Update).then(|| "1.0".to_string()),
        version: "2.0".into(),
        download_host: Some("downloads.example.com".into()),
        blocked: None,
        apps: vec![app],
    }
}

pub(super) fn outcome(status: i32, stdout: &str, stderr: &str) -> CommandOutcome {
    CommandOutcome {
        status: Some(status),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

/// `brew outdated --cask --json=v2` for `(token, installed, current, pinned)`.
pub(super) fn outdated_json(casks: &[(&str, &str, &str, bool)]) -> String {
    let casks: Vec<Value> = casks
        .iter()
        .map(|(token, installed, current, pinned)| {
            json!({
                "name": token,
                "installed_versions": [installed],
                "current_version": current,
                "pinned": pinned,
                "pinned_version": null,
            })
        })
        .collect();
    json!({"formulae": [], "casks": casks}).to_string()
}
