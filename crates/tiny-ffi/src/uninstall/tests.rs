//! Uninstall adapter tests; fixtures are in `test_support`.

use std::path::Path;
use std::sync::atomic::Ordering;

use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::protection_store::ProtectionStore;

use super::test_support::*;
use super::*;
use crate::clean::{FfiItemOutcome, FfiTrashFailure};

#[test]
fn list_reports_apps_with_opaque_ids_and_blocked_reasons() {
    let world = World::new();
    let apps = world.list();
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[0].name, APP);
    assert_eq!(apps[0].bundle_id.as_deref(), Some(ID));
    assert_eq!(apps[0].size_bytes, 4);
    assert_eq!(apps[0].last_used_days, Some(3));
    assert_eq!(apps[0].blocked_reason, None);
    assert!(apps[0].path.ends_with(BUNDLE));
    assert!(!apps[0].app_id.contains(BUNDLE), "the ID is not the path");
    assert_ne!(apps[0].app_id, apps[1].app_id);
    assert!(apps[1]
        .blocked_reason
        .as_deref()
        .unwrap()
        .contains("system app"));
    assert!(!world.session.is_busy());
}

#[test]
fn preview_classifies_every_item() {
    let world = World::new();
    let preview = world.preview();
    assert_eq!(preview.items.len(), 6);
    let class = |name| {
        (
            &item(&preview, name).ownership,
            item(&preview, name).selectable,
        )
    };
    assert_eq!(class(BUNDLE), (&FfiUninstallOwnership::App, true));
    assert_eq!(
        class("Caches/com.example.Fixture"),
        (&FfiUninstallOwnership::Cache, true)
    );
    assert_eq!(
        class("Application Support/com.example.Fixture"),
        (&FfiUninstallOwnership::Data, true)
    );
    assert_eq!(class(PREFS), (&FfiUninstallOwnership::Data, true));
    let (group, selectable) = class(GROUP);
    assert!(matches!(group, FfiUninstallOwnership::ReportOnly { .. }) && !selectable);
    let (storage, selectable) = class("HTTPStorages/com.example.Fixture");
    assert!(matches!(storage, FfiUninstallOwnership::Uncertain { .. }) && !selectable);
    assert_eq!(item(&preview, BUNDLE).kind, FfiUninstallItemKind::AppBundle);
    assert_eq!(item(&preview, PREFS).kind, FfiUninstallItemKind::Leftover);
    assert_eq!(preview.run_state, FfiAppRunState::NotRunning);
    assert_eq!(preview.expires_in_seconds, PREVIEW_TTL.as_secs());
    assert!(preview.expires_at > 0);
    let selectable_sum: u64 = preview
        .items
        .iter()
        .filter(|i| i.selectable)
        .map(|i| i.size_bytes)
        .sum();
    assert_eq!(preview.selectable_bytes, selectable_sum);
    assert!(preview
        .items
        .iter()
        .all(|i| i.item_id.starts_with(&preview.preview_id)));
    assert!(preview.items.iter().all(|i| i.held_by.is_none()));
}

#[test]
fn preview_refuses_unknown_and_blocked_apps() {
    let world = World::new();
    let early = world
        .session
        .preview_uninstall("a1-0", &world.catalog, &world.probe);
    assert!(
        matches!(early, Err(FfiError::PreviewInvalid { .. })),
        "no listing yet"
    );
    let apps = world.list();
    let forged = world
        .session
        .preview_uninstall("a99-0", &world.catalog, &world.probe);
    assert!(matches!(forged, Err(FfiError::PreviewInvalid { .. })));
    let system = apps.iter().find(|a| a.blocked_reason.is_some()).unwrap();
    let blocked = world
        .session
        .preview_uninstall(&system.app_id, &world.catalog, &world.probe);
    assert!(matches!(
        expect_refusal(blocked.unwrap_err()),
        FfiUninstallRefusal::Blocked { .. }
    ));
    // A new listing retires the old IDs.
    let old = apps[0].app_id.clone();
    world.list();
    let stale = world
        .session
        .preview_uninstall(&old, &world.catalog, &world.probe);
    assert!(matches!(stale, Err(FfiError::PreviewInvalid { .. })));
}

#[test]
fn preview_reports_a_running_or_unknown_app() {
    let world = World::new();
    world.probe.running.store(true, Ordering::SeqCst);
    assert_eq!(world.preview().run_state, FfiAppRunState::Running);
    world.probe.fails.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.preview().run_state,
        FfiAppRunState::Unknown { .. }
    ));
}

#[test]
fn forged_unknown_and_replaced_previews_move_nothing() {
    let world = World::new();
    let trash = FakeTrash::default();
    let first = world.preview();
    let second = world.preview();
    let token = CancellationToken::default();
    for id in ["u999", "", &first.preview_id] {
        let run = world.execute(&trash, id, &[&item(&second, BUNDLE).item_id], &token);
        assert!(
            matches!(run, Err(FfiError::PreviewInvalid { .. })),
            "{id:?}"
        );
    }
    assert!(trash.moved_names().is_empty());
    assert!(!world.session.is_busy());
}

#[test]
fn consumed_preview_cannot_run_again() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    world.run(&trash, &preview, &[BUNDLE]).unwrap();
    let again = world.run(&trash, &preview, &[BUNDLE]);
    assert!(
        matches!(again, Err(FfiError::PreviewInvalid { detail }) if detail.contains("already used"))
    );
    assert_eq!(trash.moved_names(), [BUNDLE]);
}

#[test]
fn expired_preview_is_refused() {
    let world = World::new();
    world.session.uninstall_state().ttl = Some(Duration::ZERO);
    let preview = world.preview();
    let trash = FakeTrash::default();
    let run = world.run(&trash, &preview, &[BUNDLE]);
    assert!(matches!(run, Err(FfiError::PreviewInvalid { detail }) if detail.contains("expired")));
    assert!(trash.moved_names().is_empty());
}

fn invalid_input(error: FfiError) -> String {
    match error {
        FfiError::InvalidInput { detail } => detail,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn bad_selections_are_invalid_input_and_do_not_spend_the_preview() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    let token = CancellationToken::default();
    for bad in ["u1-99", "x1-0"] {
        let run = world.execute(&trash, &preview.preview_id, &[bad], &token);
        assert!(invalid_input(run.unwrap_err()).contains("unknown item"));
    }
    let empty = world.execute(&trash, &preview.preview_id, &[], &token);
    assert!(invalid_input(empty.unwrap_err()).contains("nothing is selected"));
    for name in [GROUP, "HTTPStorages/com.example.Fixture"] {
        let run = world.run(&trash, &preview, &[BUNDLE, name]);
        let wanted = item(&preview, name).item_id.clone();
        assert!(invalid_input(run.unwrap_err()).contains(&wanted), "{name}");
    }
    assert!(trash.moved_names().is_empty());
    // Still spendable: none of the refusals above consumed it.
    assert_eq!(
        world.run(&trash, &preview, &[BUNDLE]).unwrap().moved_count,
        1
    );
}

#[test]
fn a_leftovers_only_selection_is_refused_before_consuming() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    let run = world.run(&trash, &preview, &[PREFS, "Caches/com.example.Fixture"]);
    assert_eq!(
        invalid_input(run.unwrap_err()),
        "select the app to uninstall it"
    );
    assert!(trash.moved_names().is_empty());
    assert_eq!(
        world.run(&trash, &preview, &[BUNDLE]).unwrap().moved_count,
        1
    );
}

#[test]
fn running_app_is_refused_and_spends_the_preview() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    world.probe.running.store(true, Ordering::SeqCst);
    let run = world.run(&trash, &preview, &[BUNDLE, "Caches/com.example.Fixture"]);
    assert!(
        matches!(expect_refusal(run.unwrap_err()), FfiUninstallRefusal::AppRunning { app } if app == EXE)
    );
    world.probe.running.store(false, Ordering::SeqCst);
    let retry = world.run(&trash, &preview, &[BUNDLE]);
    assert!(matches!(retry, Err(FfiError::PreviewInvalid { .. })));
    world.probe.fails.store(true, Ordering::SeqCst);
    let fresh = world.preview();
    let failed = world.run(&trash, &fresh, &[BUNDLE]);
    assert!(matches!(
        expect_refusal(failed.unwrap_err()),
        FfiUninstallRefusal::AppCheckFailed { .. }
    ));
    assert!(trash.moved_names().is_empty());
}

#[test]
fn a_second_copy_installed_after_the_preview_refuses_leftovers() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    // Different case: library folders resolve case-insensitively.
    let twin = app_entry(
        &world.catalog.root,
        "Fixture Copy",
        Some("com.example.fixture"),
    );
    world.catalog.apps.lock().unwrap().push(twin);
    let run = world.run(&trash, &preview, &[BUNDLE, "Caches/com.example.Fixture"]);
    assert_eq!(
        expect_refusal(run.unwrap_err()),
        FfiUninstallRefusal::DuplicateBundleId
    );
    assert!(trash.moved_names().is_empty());
}

#[test]
fn a_bundle_only_run_ignores_a_new_second_copy() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    let twin = app_entry(&world.catalog.root, "Fixture Copy", Some(ID));
    world.catalog.apps.lock().unwrap().push(twin);
    assert_eq!(
        world.run(&trash, &preview, &[BUNDLE]).unwrap().moved_count,
        1
    );
}

#[test]
fn an_unreadable_inventory_at_execute_refuses_leftovers() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    world.catalog.unreadable.store(true, Ordering::SeqCst);
    let run = world.run(&trash, &preview, &[BUNDLE, PREFS]);
    assert_eq!(
        expect_refusal(run.unwrap_err()),
        FfiUninstallRefusal::InventoryUnavailable
    );
    assert!(trash.moved_names().is_empty());
}

#[test]
fn an_unreadable_inventory_at_preview_makes_leftovers_report_only() {
    let world = World::new();
    world.list();
    world.catalog.unreadable.store(true, Ordering::SeqCst);
    let id = world.app_id();
    let preview = world
        .session
        .preview_uninstall(&id, &world.catalog, &world.probe)
        .unwrap();
    let selectable: Vec<_> = preview.items.iter().filter(|i| i.selectable).collect();
    assert_eq!(selectable.len(), 1);
    assert_eq!(selectable[0].kind, FfiUninstallItemKind::AppBundle);
}

#[test]
fn a_rule_saved_after_the_preview_stops_the_run() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    world.save_rule(&world.lib("Caches/com.example.Fixture"));
    let run = world.run(&trash, &preview, &[BUNDLE]);
    assert!(matches!(run, Err(FfiError::ProtectionChanged)));
    let again = world.run(&trash, &preview, &[BUNDLE]);
    assert!(
        matches!(again, Err(FfiError::PreviewInvalid { .. })),
        "the preview was dropped"
    );
    assert!(trash.moved_names().is_empty());
}

#[test]
fn a_held_leftover_is_shown_unselectable_and_refused_before_consuming() {
    let world = World::new();
    let held = "Application Support/com.example.Fixture";
    world.save_rule(&world.lib(held));
    let preview = world.preview();
    let data = item(&preview, held);
    assert!(matches!(
        data.held_by,
        Some(FfiProtectionGuard::Kept { .. })
    ));
    assert!(!data.selectable);
    let selectable_sum: u64 = preview
        .items
        .iter()
        .filter(|i| i.selectable)
        .map(|i| i.size_bytes)
        .sum();
    assert_eq!(preview.selectable_bytes, selectable_sum);
    let trash = FakeTrash::default();
    let run = world.run(&trash, &preview, &[BUNDLE, held]);
    assert!(invalid_input(run.unwrap_err()).contains("held by a saved rule"));
    // The preview is intact: the rest of the selection runs, the held item stays.
    let report = world
        .run(&trash, &preview, &[BUNDLE, "Caches/com.example.Fixture"])
        .unwrap();
    assert_eq!(trash.moved_names(), [BUNDLE, CACHE_NAME]);
    assert_eq!(report.stopped, None);
}

#[test]
fn a_cancelled_token_attempts_nothing() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    let token = CancellationToken::default();
    token.cancel();
    let ids = [
        item(&preview, BUNDLE).item_id.as_str(),
        item(&preview, PREFS).item_id.as_str(),
    ];
    let report = world
        .execute(&trash, &preview.preview_id, &ids, &token)
        .unwrap();
    assert_eq!(report.stopped, Some(FfiUninstallStop::Cancelled));
    assert!(report
        .results
        .iter()
        .all(|r| r.outcome == FfiItemOutcome::NotAttempted));
    assert_eq!(report.not_attempted_count, 2);
    assert!(trash.moved_names().is_empty());
}

#[test]
fn cancelling_mid_run_stops_the_rest() {
    let world = World::new();
    let preview = world.preview();
    let token = CancellationToken::default();
    let trash = FakeTrash {
        on_move: Some(Box::new(|_| token.cancel())),
        ..FakeTrash::default()
    };
    let ids = [
        item(&preview, BUNDLE).item_id.as_str(),
        item(&preview, PREFS).item_id.as_str(),
    ];
    let report = world
        .execute(&trash, &preview.preview_id, &ids, &token)
        .unwrap();
    assert_eq!(report.stopped, Some(FfiUninstallStop::Cancelled));
    assert_eq!((report.moved_count, report.not_attempted_count), (1, 1));
    assert_eq!(trash.moved_names(), [BUNDLE]);
}

#[test]
fn an_automation_denial_stops_the_run() {
    let world = World::new();
    let preview = world.preview();
    let trash = FakeTrash {
        fail_on: Some((BUNDLE, TrashError::AutomationDenied("no".into()))),
        ..FakeTrash::default()
    };
    let report = world.run(&trash, &preview, &[BUNDLE, PREFS]).unwrap();
    assert_eq!(report.stopped, Some(FfiUninstallStop::AutomationDenied));
    assert!(matches!(
        outcome(&report, &preview, BUNDLE),
        FfiItemOutcome::Failed {
            reason: FfiTrashFailure::AutomationDenied,
            source_still_present: true,
            ..
        }
    ));
    assert_eq!(
        outcome(&report, &preview, PREFS),
        &FfiItemOutcome::NotAttempted
    );
    assert!(trash.moved_names().is_empty());
}

#[test]
fn a_failed_bundle_leaves_every_leftover_untouched() {
    let world = World::new();
    let preview = world.preview();
    let trash = FakeTrash {
        fail_on: Some((BUNDLE, TrashError::Failed("busy".into()))),
        ..FakeTrash::default()
    };
    let report = world.run(&trash, &preview, &[PREFS, BUNDLE]).unwrap();
    assert_eq!(report.stopped, None);
    assert_eq!((report.failed_count, report.not_attempted_count), (1, 1));
    assert!(trash.moved_names().is_empty());
}

#[test]
fn the_happy_path_moves_only_the_selected_items_bundle_first() {
    let world = World::new();
    let preview = world.preview();
    let trash = FakeTrash::default();
    // Selected out of plan order; the data folder and the others stay out.
    let report = world
        .run(
            &trash,
            &preview,
            &[PREFS, "Caches/com.example.Fixture", BUNDLE],
        )
        .unwrap();
    assert_eq!(trash.moved_names(), [BUNDLE, CACHE_NAME, PREFS]);
    assert_eq!(report.moved_count, 3);
    assert_eq!(report.results.len(), 3);
    assert_eq!(report.results[0].kind, FfiUninstallItemKind::AppBundle);
    assert_eq!(report.bytes_moved_to_trash, report.bytes_selected);
    assert_eq!(report.stopped, None);
    let untouched = world.lib("Application Support/com.example.Fixture");
    assert!(untouched.exists(), "the data folder was not selected");
    let progress = world.listener.0.lock().unwrap();
    assert!(progress.iter().all(|p| p.operation == "uninstall"));
    assert_eq!(progress.last().unwrap().completed, 3);
    assert!(!world.session.is_busy());
}

#[test]
fn uninstall_shares_the_gate_with_storage_and_clean() {
    let world = World::new();
    let preview = world.preview();
    // Another operation holds the gate: every uninstall call is Busy, and a
    // Busy execute does not spend the preview.
    let held = world.session.begin().unwrap();
    assert!(matches!(
        world.session.list_installed_apps(&world.catalog),
        Err(FfiError::Busy)
    ));
    let apps_id = world.app_id();
    let busy_preview = world
        .session
        .preview_uninstall(&apps_id, &world.catalog, &world.probe);
    assert!(matches!(busy_preview, Err(FfiError::Busy)));
    let trash = FakeTrash::default();
    assert!(matches!(
        world.run(&trash, &preview, &[BUNDLE]),
        Err(FfiError::Busy)
    ));
    drop(held);
    // While uninstall runs, a storage scan or a second operation is Busy.
    let probe_gate = |_: &Path| {
        let scan = world.session.scan_storage(
            &world.root,
            &CancellationToken::default(),
            &Listener::default(),
        );
        assert!(matches!(scan, Err(FfiError::Busy)));
        assert!(matches!(world.session.begin(), Err(FfiError::Busy)));
        assert!(world.session.is_busy());
    };
    let trash = FakeTrash {
        on_move: Some(Box::new(probe_gate)),
        ..FakeTrash::default()
    };
    assert_eq!(
        world.run(&trash, &preview, &[BUNDLE]).unwrap().moved_count,
        1
    );
}

#[test]
fn an_untrusted_home_refuses_preview_and_execute() {
    let world = World::new();
    let preview = world.preview();
    world
        .session
        .inject_clean_inputs(&world.root, &world.home, world.store_dir.clone());
    let trash = FakeTrash::default();
    let run = world.run(&trash, &preview, &[BUNDLE]);
    assert!(matches!(run, Err(FfiError::UntrustedHome { .. })));
    let id = world.app_id();
    let again = world
        .session
        .preview_uninstall(&id, &world.catalog, &world.probe);
    assert!(matches!(again, Err(FfiError::UntrustedHome { .. })));
    // Nothing was spent: the preview runs once HOME is trusted.
    world
        .session
        .inject_clean_inputs(&world.home, &world.home, world.store_dir.clone());
    assert_eq!(
        world.run(&trash, &preview, &[BUNDLE]).unwrap().moved_count,
        1
    );
}

fn add_app(world: &World, name: &str, id: Option<&str>) {
    let entry = app_entry(&world.catalog.root, name, id);
    std::fs::create_dir_all(&entry.path).unwrap();
    world.catalog.apps.lock().unwrap().push(entry);
}

fn preview_of(world: &World, name: &str) -> Result<FfiUninstallPreview, FfiError> {
    let apps = world.list();
    let app = apps.iter().find(|a| a.name == name).unwrap();
    world
        .session
        .preview_uninstall(&app.app_id, &world.catalog, &world.probe)
}

#[test]
fn a_missing_or_invalid_bundle_id_searches_no_leftovers() {
    let world = World::new();
    add_app(&world, "Zed Noid", None);
    add_app(&world, "Zed Badid", Some("bad/id"));
    for name in ["Zed Noid", "Zed Badid"] {
        let preview = preview_of(&world, name).unwrap();
        assert!(!preview.leftovers_searched, "{name}");
        assert!(preview.leftovers_skipped_reason.is_some(), "{name}");
        assert_eq!(preview.items.len(), 1);
        assert_eq!(preview.items[0].ownership, FfiUninstallOwnership::App);
    }
    assert_eq!(
        preview_of(&world, "Zed Badid")
            .unwrap()
            .bundle_id
            .as_deref(),
        Some("bad/id")
    );
    assert_eq!(preview_of(&world, "Zed Noid").unwrap().bundle_id, None);
    let normal = world.preview();
    assert!(normal.leftovers_searched);
    assert_eq!(normal.leftovers_skipped_reason, None);
    assert_eq!(normal.bundle_id.as_deref(), Some(ID));
}

#[test]
fn an_unknown_process_name_shows_unknown_and_refuses_execution() {
    let world = World::new();
    *world.catalog.executable.lock().unwrap() = None;
    let preview = world.preview();
    assert!(matches!(preview.run_state, FfiAppRunState::Unknown { .. }));
    let trash = FakeTrash::default();
    let run = world.run(&trash, &preview, &[BUNDLE]);
    assert_eq!(
        expect_refusal(run.unwrap_err()),
        FfiUninstallRefusal::ExecutableUnknown
    );
    assert!(trash.moved_names().is_empty());
}

#[test]
fn an_app_removed_since_the_listing_cannot_be_previewed() {
    let world = World::new();
    let id = world.list()[0].app_id.clone();
    world.catalog.apps.lock().unwrap().clear();
    let run = world
        .session
        .preview_uninstall(&id, &world.catalog, &world.probe);
    assert!(
        matches!(run, Err(FfiError::PreviewInvalid { detail }) if detail.contains("no longer installed"))
    );
}

#[test]
fn a_busy_rules_lease_does_not_spend_the_preview() {
    let world = World::new();
    let trash = FakeTrash::default();
    let preview = world.preview();
    let store = ProtectionStore::at(world.store_dir.clone());
    let lease = store.lease().unwrap();
    assert!(matches!(
        world.run(&trash, &preview, &[BUNDLE]),
        Err(FfiError::Busy)
    ));
    drop(lease);
    assert_eq!(
        world.run(&trash, &preview, &[BUNDLE]).unwrap().moved_count,
        1
    );
}

#[test]
fn an_app_starting_mid_run_stops_the_rest() {
    let world = World::new();
    let preview = world.preview();
    let trash = FakeTrash {
        on_move: Some(Box::new(|_| {
            world.probe.running.store(true, Ordering::SeqCst)
        })),
        ..FakeTrash::default()
    };
    let report = world.run(&trash, &preview, &[BUNDLE, PREFS]).unwrap();
    assert_eq!(
        report.stopped,
        Some(FfiUninstallStop::AppRunning { app: EXE.into() })
    );
    assert_eq!((report.moved_count, report.not_attempted_count), (1, 1));
    assert_eq!(trash.moved_names(), [BUNDLE]);
}
