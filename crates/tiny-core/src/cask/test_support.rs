//! Fixtures for the cask tests: `brew info` JSON built to the shape Homebrew
//! 7 prints, and a fake `brew` path. No test runs a real `brew`.

use serde_json::{json, Value};

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
