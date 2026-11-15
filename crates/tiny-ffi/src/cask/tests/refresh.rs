//! `cask_refresh_index`: `brew update` in the cask scan slot, never an error
//! for contention, a lost network or a timeout.

use std::sync::mpsc::channel;

use crate::session::ScanSlot;

use super::*;

const UPDATE: &str = "update";

fn refresh(world: &World) -> Result<FfiIndexRefresh, FfiCaskError> {
    world.session.refresh_index(&world.seams())
}

#[test]
fn a_finished_brew_update_is_refreshed_and_keeps_the_update_list() {
    let world = updatable();
    world.brew.clear_calls();
    world.brew.reply(UPDATE, 0, "Already up-to-date.\n", "");
    assert_eq!(refresh(&world).unwrap(), FfiIndexRefresh::Refreshed);
    assert_eq!(world.brew.calls(), [UPDATE]);
    assert!(!world.session.is_busy(), "the slot is released");
    // The list a preview checks tokens against survives the refresh.
    preview_update(&world, &["ghostty"]);
}

#[test]
fn homebrews_lock_defers_and_a_lost_network_ends_without_an_error() {
    let world = World::new();
    world.brew.reply(
        UPDATE,
        1,
        "",
        "Error: Another `brew update` process is already running.\n",
    );
    assert_eq!(refresh(&world).unwrap(), FfiIndexRefresh::Deferred);
    world.brew.reply(
        UPDATE,
        1,
        "",
        "curl: (6) Could not resolve host: formulae.brew.sh\n",
    );
    let FfiIndexRefresh::Ended { failure } = refresh(&world).unwrap() else {
        panic!("a lost network ends the refresh");
    };
    assert_eq!(failure.kind, FfiCaskFailureKind::Network);
}

#[test]
fn no_homebrew_is_the_only_error_and_spawns_nothing() {
    let world = World::new();
    *world.brew.path.lock().unwrap() = None;
    assert_eq!(
        failure_kind(refresh(&world).unwrap_err()),
        FfiCaskFailureKind::HomebrewMissing
    );
    assert!(world.brew.calls().is_empty());
}

#[test]
fn a_write_or_a_cask_list_holding_the_gate_defers_the_refresh_without_brew() {
    let world = World::new();
    {
        let _write = world.session.begin().unwrap();
        assert_eq!(refresh(&world).unwrap(), FfiIndexRefresh::Deferred);
    }
    {
        let _list = world.session.begin_scan(ScanSlot::Casks).unwrap();
        assert_eq!(refresh(&world).unwrap(), FfiIndexRefresh::Deferred);
    }
    assert!(world.brew.calls().is_empty(), "brew update never started");
}

#[test]
fn while_brew_update_runs_lists_previews_runs_and_writes_are_busy_and_other_scans_go_on() {
    let world = updatable();
    world.brew.reply(UPDATE, 0, "", "");
    let preview = preview_update(&world, &["ghostty"]);
    let (entered, entered_rx) = channel();
    let (release, release_rx) = channel();
    *world.brew.hold.lock().unwrap() = Some(Hold {
        args: UPDATE.into(),
        entered,
        release: release_rx,
    });
    std::thread::scope(|scope| {
        let running = scope.spawn(|| refresh(&world));
        entered_rx.recv().expect("brew update started");
        assert!(matches!(
            world.session.outdated(&world.seams()),
            Err(FfiCaskError::Busy)
        ));
        assert!(matches!(
            world.session.catalogue(&world.seams()),
            Err(FfiCaskError::Busy)
        ));
        let tokens = vec!["ghostty".to_string()];
        assert!(matches!(
            world.session.update_preview(&tokens, &world.seams()),
            Err(FfiCaskError::Busy)
        ));
        assert!(matches!(
            world.exec(&preview.preview_id, &["ghostty"], &token()),
            Err(FfiCaskError::Busy)
        ));
        assert!(world.session.begin().is_err(), "a write waits too");
        assert!(
            world.session.begin_scan(ScanSlot::Storage).is_ok(),
            "another feature's scan is not held up"
        );
        release.send(()).unwrap();
        assert_eq!(running.join().unwrap().unwrap(), FfiIndexRefresh::Refreshed);
    });
    assert!(!world.session.is_busy());
}
