use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::cask::test_support::*;
use crate::cask::Stage;
use crate::runner::test_support::MockRunner;
use crate::runner::{CommandError, CommandOutcome};

fn info_call(token: &str) -> String {
    format!("{BREW_PATH} info --cask --json=v2 {token}")
}

fn plan(runner: &MockRunner, token: &str) -> Result<CaskPlanItem, CaskFailure> {
    plan_install(&Brew::new(Path::new(BREW_PATH), runner), token, &[])
}

fn installed(name: &str, bundle_id: Option<&str>) -> AppEntry {
    AppEntry {
        name: name.into(),
        path: PathBuf::from(format!("/Applications/{name}.app")),
        bundle_id: bundle_id.map(String::from),
        size: 0,
        last_used_days: None,
    }
}

fn runner_for(token: &str, cask: serde_json::Value) -> MockRunner {
    MockRunner::new().with_exit(
        BREW_PATH,
        &["info", "--cask", "--json=v2", token],
        0,
        &info_json(&[cask]),
    )
}

/// A catalogue cask whose app would land under `dir`, not in `/Applications`.
fn rectangle_in(dir: &Path) -> serde_json::Value {
    cask(
        "rectangle",
        "2.0.3",
        json!([{"app": ["Rectangle.app"], "target": dir.join("Rectangle.app")}]),
    )
}

#[test]
fn only_a_catalogue_token_is_planned_and_nothing_spawns_for_any_other() {
    let runner = MockRunner::new();
    for token in [
        "wireshark",
        "Rectangle",
        "--force",
        "user/tap/rectangle",
        "",
    ] {
        assert_eq!(
            plan(&runner, token),
            Err(CaskFailure::NotInCatalogue {
                token: token.into()
            }),
            "{token:?}"
        );
    }
    assert!(runner.output_calls.lock().unwrap().is_empty());
}

#[test]
fn the_plan_names_the_version_host_command_and_app() {
    let dir = tempfile::tempdir().unwrap();
    let runner = runner_for("rectangle", rectangle_in(dir.path()));
    let item = plan(&runner, "rectangle").unwrap();
    assert_eq!(item.action, CaskAction::Install);
    assert_eq!(item.token, "rectangle");
    assert_eq!(
        item.name, "Rectangle",
        "the catalogue's name, not the cask's"
    );
    assert_eq!(item.from_version, None);
    assert_eq!(item.version, "2.0.3");
    assert_eq!(item.download_host.as_deref(), Some("downloads.example.com"));
    assert_eq!(item.command(), "brew install --cask rectangle");
    assert_eq!(item.blocked, None);
    assert_eq!(item.apps, [dir.path().join("Rectangle.app")]);
    assert_eq!(
        *runner.output_calls.lock().unwrap(),
        [info_call("rectangle")]
    );
}

#[test]
fn a_cask_that_needs_a_password_is_planned_so_the_sheet_can_say_so() {
    let needy = cask(
        "rectangle",
        "2.0.3",
        json!([{"pkg": ["Rectangle.pkg"]}, {"uninstall": [{"pkgutil": "com.example.pkg"}]}]),
    );
    let item = plan(&runner_for("rectangle", needy), "rectangle").unwrap();
    assert_eq!(item.blocked, Some(CaskFailure::NeedsAdministrator));
    assert_eq!(
        item.refusal(),
        Some(CaskFailure::NeedsAdministrator),
        "the run refuses it"
    );
}

#[test]
fn only_a_pkg_blocks_an_install_not_the_uninstall_it_would_run_on_update() {
    // Docker Desktop's shape: installs with no password, its update needs one.
    let dir = tempfile::tempdir().unwrap();
    let docker = cask(
        "rectangle",
        "2.0.3",
        json!([{"uninstall": [{"delete": "/Library/PrivilegedHelperTools/x"}]},
               {"app": ["Rectangle.app"], "target": dir.path().join("Rectangle.app")}]),
    );
    let item = plan(&runner_for("rectangle", docker), "rectangle").unwrap();
    assert_eq!(item.blocked, None);
}

#[test]
fn a_manual_installer_is_planned_as_blocked_with_its_own_reason() {
    let manual = cask(
        "rectangle",
        "2.0.3",
        json!([{"installer": [{"manual": "Setup.app"}]}]),
    );
    let item = plan(&runner_for("rectangle", manual), "rectangle").unwrap();
    assert_eq!(
        item.refusal(),
        Some(CaskFailure::ManualInstaller {
            token: "rectangle".into()
        })
    );
}

#[test]
fn a_build_number_after_a_comma_is_left_off_the_shown_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut listed = rectangle_in(dir.path());
    listed["version"] = json!("0.4.26,4");
    let item = plan(&runner_for("rectangle", listed), "rectangle").unwrap();
    assert_eq!(item.version, "0.4.26");
}

#[test]
fn an_app_with_the_same_bundle_identity_stops_the_plan_before_brew_is_asked() {
    let runner = MockRunner::new();
    // A moved and renamed copy still counts; a look-alike name does not.
    let moved = installed("My Snapper", Some("com.knollsoft.Rectangle"));
    let failure = plan_install(
        &Brew::new(Path::new(BREW_PATH), &runner),
        "rectangle",
        &[moved],
    );
    assert_eq!(
        failure,
        Err(CaskFailure::AppExists {
            path: "/Applications/My Snapper.app".into()
        })
    );
    assert!(runner.output_calls.lock().unwrap().is_empty());

    let dir = tempfile::tempdir().unwrap();
    let runner = runner_for("rectangle", rectangle_in(dir.path()));
    let unrelated = [
        installed("Rectangle", Some("com.example.Other")),
        installed("Unreadable", None),
    ];
    let planned = plan_install(
        &Brew::new(Path::new(BREW_PATH), &runner),
        "rectangle",
        &unrelated,
    );
    assert!(planned.is_ok(), "{planned:?}");
}

#[test]
fn an_app_already_where_the_cask_would_go_stops_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("Rectangle.app")).unwrap();
    let failure = plan(
        &runner_for("rectangle", rectangle_in(dir.path())),
        "rectangle",
    );
    assert_eq!(
        failure,
        Err(CaskFailure::AppExists {
            path: dir
                .path()
                .join("Rectangle.app")
                .to_string_lossy()
                .into_owned()
        })
    );
}

#[test]
fn a_cask_homebrew_already_lists_is_not_planned_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut listed = rectangle_in(dir.path());
    listed["installed"] = json!("2.0.1");
    assert_eq!(
        plan(&runner_for("rectangle", listed), "rectangle"),
        Err(CaskFailure::AlreadyInstalled {
            token: "rectangle".into()
        })
    );
}

#[test]
fn brew_failures_and_empty_answers_are_typed() {
    let args = ["info", "--cask", "--json=v2", "rectangle"];
    let with = |result| MockRunner::new().with_output(BREW_PATH, &args, result);
    assert_eq!(
        plan(
            &with(Err(CommandError::NotFound(BREW_PATH.into()))),
            "rectangle"
        ),
        Err(CaskFailure::HomebrewMissing)
    );
    assert_eq!(
        plan(
            &MockRunner::new().with_exit(BREW_PATH, &args, 0, &info_json(&[])),
            "rectangle"
        ),
        Err(CaskFailure::CaskNotFound {
            token: "rectangle".into()
        })
    );
    let refused = Ok(CommandOutcome {
        status: Some(1),
        stdout: String::new(),
        stderr: "Error: Cask 'rectangle' is unavailable: No Cask with this name exists.".into(),
    });
    assert_eq!(
        plan(&with(refused), "rectangle"),
        Err(CaskFailure::CaskNotFound {
            token: "rectangle".into()
        })
    );
}

#[test]
fn running_an_install_reports_each_stage_with_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let item = plan(
        &runner_for("rectangle", rectangle_in(dir.path())),
        "rectangle",
    )
    .unwrap();
    let runner = MockRunner::new().with_exit(
        BREW_PATH,
        &["install", "--cask", "rectangle"],
        0,
        "==> Installing Cask rectangle\n",
    );
    let seen = Mutex::new(Vec::new());
    let report = |p: CaskProgress| seen.lock().unwrap().push(p);
    let outcome = run_install(
        &Brew::new(Path::new(BREW_PATH), &runner),
        &item,
        None,
        Some(&report),
    );
    assert_eq!(outcome, RunOutcome::Done);
    let step = |stage| CaskProgress {
        token: "rectangle".into(),
        stage,
        position: 0,
        total: 1,
    };
    assert_eq!(
        seen.into_inner().unwrap(),
        [step(Stage::Working), step(Stage::Installing)]
    );
}
