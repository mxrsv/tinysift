use serde_json::{json, Value};

use super::*;

fn of(artifacts: Value) -> Elevation {
    Elevation::of(artifacts.as_array().unwrap())
}

const NONE: Elevation = Elevation {
    install: false,
    manual_installer: false,
    update: false,
};

#[test]
fn the_catalogue_casks_need_nothing_even_visual_studio_codes_launchctl() {
    // Artifacts as `brew info --json=v2` prints them for the catalogue.
    let vscode = json!([
        {"uninstall": [{"launchctl": "com.microsoft.VSCode.ShipIt", "quit": "com.microsoft.VSCode"}]},
        {"app": ["Visual Studio Code.app"], "target": "/Applications/Visual Studio Code.app"},
        {"binary": ["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"],
         "target": "/opt/homebrew/bin/code"},
    ]);
    let rectangle = json!([
        {"uninstall": [{"quit": "com.knollsoft.Rectangle", "login_item": "Rectangle"}]},
        {"app": ["Rectangle.app"], "target": "/Applications/Rectangle.app"},
    ]);
    let brave =
        json!([{"app": ["Brave Browser.app"], "target": "/Applications/Brave Browser.app"}]);
    for artifacts in [vscode, rectangle, brave] {
        assert_eq!(of(artifacts), NONE);
    }
}

#[test]
fn signal_trash_and_launchctl_stop_directives_need_no_password() {
    // `launchctl`, `signal` and `trash` in an `uninstall` never ask for a password.
    let toolbox = json!([
        {"uninstall": [{"launchctl": "com.jetbrains.toolbox", "quit": "com.jetbrains.toolbox",
                        "signal": ["TERM", "com.jetbrains.toolbox"]}]},
        {"app": ["JetBrains Toolbox.app"], "target": "/Applications/JetBrains Toolbox.app"},
    ]);
    let dbeaver = json!([
        {"uninstall": [{"signal": ["TERM", "org.jkiss.dbeaver.core.product"]}]},
        {"app": ["DBeaver.app"], "target": "/Applications/DBeaver.app"},
        {"binary": ["/Applications/DBeaver.app/Contents/MacOS/dbeaver"], "target": "/opt/homebrew/bin/dbeaver"},
    ]);
    let podman = json!([
        {"uninstall": [{"quit": "io.podmandesktop.PodmanDesktop",
                        "trash": "~/Library/LaunchAgents/io.podman_desktop.PodmanDesktop.plist"}]},
        {"app": ["Podman Desktop.app"], "target": "/Applications/Podman Desktop.app"},
    ]);
    for artifacts in [toolbox, dbeaver, podman] {
        assert_eq!(of(artifacts), NONE);
    }
}

#[test]
fn a_pkg_or_keyboard_layout_needs_sudo_to_install_and_so_to_update() {
    for artifact in ["pkg", "keyboard_layout"] {
        let elevation = of(json!([{ artifact: ["Thing.pkg"] }]));
        assert_eq!(
            elevation,
            Elevation {
                install: true,
                manual_installer: false,
                update: true
            },
            "{artifact}"
        );
    }
}

#[test]
fn an_uninstall_that_always_uses_sudo_blocks_the_update_but_not_the_install() {
    // Docker Desktop: installs with no password, but its update runs
    // `uninstall delete:` on the old copy.
    let docker = json!([
        {"uninstall": [{"launchctl": ["com.docker.helper"], "quit": ["com.docker.docker"],
                        "delete": ["/Library/PrivilegedHelperTools/com.docker.vmnetd"],
                        "rmdir": "~/.docker/bin"}]},
        {"app": ["Docker.app"], "target": "/Applications/Docker.app"},
        {"postflight_steps": [{"steps": [{"type": "set_ownership", "sudo": "if_needed"}]}]},
    ]);
    assert_eq!(
        of(docker),
        Elevation {
            install: false,
            manual_installer: false,
            update: true
        }
    );
    for directive in [
        json!({"pkgutil": "com.example.pkg"}),
        json!({"kext": "com.example.kext"}),
        json!({"delete": "/Library/Thing"}),
        json!({"script": {"executable": "x", "sudo": true}}),
        json!({"early_script": {"executable": "x", "sudo": true}}),
    ] {
        let elevation = of(json!([{"app": ["A.app"]}, {"uninstall": [directive.clone()]}]));
        assert!(!elevation.install && elevation.update, "{directive}");
    }
}

#[test]
fn directives_that_do_not_force_sudo_leave_the_update_alone() {
    for directive in [
        json!({"launchctl": ["a", "b"]}),
        json!({"quit": "com.example"}),
        json!({"signal": ["KILL", "com.example"]}),
        json!({"login_item": "Example"}),
        json!({"rmdir": "~/Library/Example"}),
        json!({"trash": "~/Library/Example"}),
        json!({"script": {"executable": "x", "sudo": false}}),
        json!({"script": {"executable": "x"}}),
    ] {
        let elevation = of(json!([{"app": ["A.app"]}, {"uninstall": [directive.clone()]}]));
        assert_eq!(elevation, NONE, "{directive}");
    }
}

#[test]
fn an_installer_script_needs_sudo_only_when_it_says_so() {
    let with = |sudo: Value| {
        of(
            json!([{"installer": [{"script": {"executable": "setup", "args": ["-silent"], "sudo": sudo}}]}]),
        )
    };
    assert!(with(json!(true)).install);
    assert!(!with(json!(false)).install);
}

#[test]
fn a_manual_installer_is_its_own_case_and_installs_nothing() {
    let adobe = json!([
        {"uninstall": [{"delete": "/Applications/Adobe Connect"}]},
        {"installer": [{"manual": "AdobeConnectInstaller.app"}]},
    ]);
    let elevation = of(adobe);
    assert!(elevation.manual_installer);
    assert!(
        !elevation.install,
        "it asks for no password; it just cannot run"
    );
}

#[test]
fn install_steps_count_when_sudo_is_true_or_they_delete_a_keychain_certificate() {
    for step in [
        json!({"type": "set_ownership", "sudo": true}),
        json!({"type": "delete_keychain_certificate", "name": "x"}),
    ] {
        for key in ["preflight_steps", "postflight_steps"] {
            let elevation = of(json!([{"app": ["A.app"]}, {key: [{"steps": [step.clone()]}]}]));
            assert!(elevation.install && elevation.update, "{key} {step}");
        }
    }
    let soft = json!([{"postflight_steps": [{"steps": [{"type": "set_ownership", "sudo": "if_needed"}]}]}]);
    assert_eq!(of(soft), NONE, "`if_needed` is left to HOMEBREW_NO_SUDO");
}

#[test]
fn uninstall_steps_with_sudo_block_the_update_only() {
    let elevation = of(json!([
        {"app": ["A.app"]},
        {"uninstall_postflight_steps": [{"steps": [{"type": "x", "sudo": true}]}]},
    ]));
    assert_eq!(
        elevation,
        Elevation {
            install: false,
            manual_installer: false,
            update: true
        }
    );
}

#[test]
fn anything_unreadable_asks_for_nothing() {
    assert_eq!(Elevation::of(&[]), NONE);
    assert_eq!(of(json!(["text", 3, null, {"zap": "<trimmed>"}])), NONE);
}
