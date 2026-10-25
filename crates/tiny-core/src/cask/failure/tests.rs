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
fn a_checksum_mismatch_means_homebrews_data_is_old_not_that_the_app_is_bad() {
    let text = "Error: Cask reports different checksum: 9f2c\n\
                SHA-256 checksum of downloaded file: 77ab\n";
    let failure = classify_exit("ghostty", &outcome(1, text));
    assert_eq!(failure, CaskFailure::StaleData);
    assert!(failure.recovery().contains("brew update"));
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
        CaskFailure::TimedOut { running: true }
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
        CaskFailure::TimedOut { running: true },
        CaskFailure::TimedOut { running: false },
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

#[test]
fn something_else_in_the_way_is_not_called_an_app() {
    for text in [
        "Error: It seems there is already a Binary at '/opt/homebrew/bin/code'.",
        "Error: It seems there is already a Manpage at '/opt/homebrew/share/man/man1/x.1'.",
        "Error: It seems there is already an App Suite at '/Applications/Suite'.",
    ] {
        let failure = classify_exit("x", &outcome(1, text));
        assert!(
            matches!(&failure, CaskFailure::BrewFailed { detail, .. } if detail.contains("there is already")),
            "{text}: {failure:?}"
        );
        assert!(!failure.problem().contains("app already sits"), "{text}");
    }
}

#[test]
fn a_timeout_is_told_as_a_download_or_as_a_local_call() {
    let download = CaskFailure::TimedOut { running: true };
    let local = CaskFailure::TimedOut { running: false };
    assert!(download.recovery().contains("faster connection"));
    assert!(
        !local.recovery().contains("connection"),
        "info and outdated read local files"
    );
    assert!(local.recovery().contains("brew doctor"));
    assert_ne!(download.problem(), local.problem());
}

#[test]
fn a_cask_homebrew_still_records_tells_how_to_clear_the_record_in_terminal() {
    let recovery = CaskFailure::AlreadyInstalled {
        token: "ghostty".into(),
    }
    .recovery();
    assert!(recovery.contains("`brew uninstall --cask ghostty`"));
    assert!(recovery.contains("Terminal"));
}
