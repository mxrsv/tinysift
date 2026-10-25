//! Cancel in the middle of a run, two threads on the gate, and the two clocks
//! of a preview: what the one-shot tests cannot show.

use std::sync::mpsc::channel;

use super::install::install_ok;

use super::*;

/// The streamed reply of `install --cask ghostty`: a download heading on stderr,
/// then the install headings on stdout.
fn streamed_install(world: &World) {
    world.brew.reply(
        INSTALL,
        0,
        "==> Installing Cask ghostty\n==> Moving App 'Ghostty.app'\nghostty was successfully installed!\n",
        "==> Fetching downloads for: ghostty\n",
    );
    *world.brew.pace.lock().unwrap() = Duration::from_millis(80);
}

#[test]
fn a_cancel_during_the_download_stops_the_run_before_it_installs() {
    let world = installable();
    streamed_install(&world);
    let preview = preview_install(&world);
    let cancel = token();
    *world.listener.cancel_at_stage.lock().unwrap() =
        Some((FfiCaskStage::Downloading, cancel.clone()));
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &cancel)
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.results[0].outcome, FfiCaskOutcome::Cancelled);
    let stages: Vec<_> = world.steps().iter().map(|s| s.1).collect();
    assert_eq!(stages, [FfiCaskStage::Working, FfiCaskStage::Downloading]);
}

#[test]
fn a_cancel_once_it_is_installing_lets_that_cask_finish() {
    let world = installable();
    streamed_install(&world);
    let preview = preview_install(&world);
    let cancel = token();
    *world.listener.cancel_at_stage.lock().unwrap() =
        Some((FfiCaskStage::Installing, cancel.clone()));
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &cancel)
        .unwrap();
    assert_eq!(report.results[0].outcome, FfiCaskOutcome::Done);
    assert!(!report.cancelled, "nothing was left undone");
    assert!(cancel.is_cancelled());
}

#[test]
fn a_cancel_while_the_first_update_installs_finishes_it_and_leaves_the_rest_alone() {
    let world = updatable();
    for token in ["ghostty", "iterm2"] {
        world.brew.reply(
            &format!("upgrade --cask --no-quit {token}"),
            0,
            "==> Backing App 'x.app' up\n==> Moving App 'x.app'\n",
            "",
        );
    }
    *world.brew.pace.lock().unwrap() = Duration::from_millis(80);
    let preview = preview_update(&world, &["ghostty", "iterm2"]);
    // `preview_update` read `info` for both at once; each run reads its own.
    let cancel = token();
    *world.listener.cancel_at_stage.lock().unwrap() =
        Some((FfiCaskStage::Installing, cancel.clone()));
    let report = world
        .exec(&preview.preview_id, &["ghostty", "iterm2"], &cancel)
        .unwrap();
    assert_eq!(report.results[0].outcome, FfiCaskOutcome::Done);
    assert_eq!(report.results[1].outcome, FfiCaskOutcome::NotAttempted);
    assert!(report.cancelled, "a cask never started");
    assert_eq!(world.brew.spawned("upgrade"), 1);
}

#[test]
fn a_second_thread_is_told_busy_while_one_holds_the_gate_inside_brew() {
    let world = updatable();
    let (entered, entered_rx) = channel();
    let (release, release_rx) = channel();
    *world.brew.hold.lock().unwrap() = Some(Hold {
        args: "outdated --cask --json=v2".into(),
        entered,
        release: release_rx,
    });
    std::thread::scope(|scope| {
        let listing = scope.spawn(|| world.session.outdated(&world.seams()));
        entered_rx.recv().expect("the first call reached brew");
        assert!(world.session.is_busy());
        assert!(matches!(
            world.session.catalogue(&world.seams()),
            Err(FfiCaskError::Busy)
        ));
        let second = world.session.install_preview("ghostty", &world.seams());
        assert!(matches!(second, Err(FfiCaskError::Busy)));
        let run = world.exec("k1", &["ghostty"], &token());
        assert!(matches!(run, Err(FfiCaskError::Busy)));
        release.send(()).unwrap();
        assert!(listing.join().unwrap().is_ok());
    });
    assert!(
        !world.session.is_busy(),
        "released once the first call ended"
    );
    *world.brew.hold.lock().unwrap() = None;
    assert!(
        world.session.outdated(&world.seams()).is_ok(),
        "usable again"
    );
}

#[test]
fn a_preview_expires_by_either_clock() {
    // The wall clock keeps running while the Mac sleeps and `Instant` does
    // not, so each can be past while the other is not.
    for by_wall_clock in [true, false] {
        let world = installable();
        install_ok(&world);
        let preview = preview_install(&world);
        {
            let mut state = world.session.cask_state();
            let stored = state.preview.as_mut().unwrap();
            if by_wall_clock {
                stored.expires_at_wall = SystemTime::now() - Duration::from_secs(1);
            } else {
                stored.expires_at = Instant::now() - Duration::from_secs(1);
            }
        }
        let error = world
            .exec(&preview.preview_id, &["ghostty"], &token())
            .unwrap_err();
        assert!(
            matches!(&error, FfiCaskError::PreviewInvalid { detail } if detail == "preview expired"),
            "{error:?}"
        );
        assert_eq!(world.brew.spawned("install"), 0);
    }
}
