//! What the keys do to the engine, through the real frame: nothing ticked on
//! open, refused rows, the action menu and the consent it passes, the
//! hard-delete box, dry-run, the outcome per app, and stopping or quitting
//! during a run. The fake engine removes nothing.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use tiny_core::uninstall::Consent;

use super::fake_engine::{Call, FakeEngine, APPLE_REFUSAL};
use super::UninstallPage;
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};
use crate::tui::pages::{all_pages, register, PageId};

fn open(engine: FakeEngine, size: (u16, u16)) -> (PageHarness, Arc<FakeEngine>) {
    let engine = Arc::new(engine);
    let screen = PageHarness::new(UninstallPage::new(engine.clone()), size, IconMode::Unicode);
    (screen, engine)
}

fn line_with(screen: &mut PageHarness, needle: &str) -> String {
    let text = screen.text();
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in\n{text}"))
        .to_string()
}

/// Ticks AltTab, and Pixelmator Pro below it when `both`.
fn tick(screen: &mut PageHarness, both: bool) {
    screen.press(KeyCode::Char(' ')).settle();
    if both {
        screen.press(KeyCode::Down).settle();
        screen.press(KeyCode::Char(' ')).settle();
    }
}

/// Opens the action menu on the picked apps and chooses the row at `choice`.
fn choose(screen: &mut PageHarness, choice: usize) {
    screen.press(KeyCode::Enter).settle();
    for _ in 0..choice {
        screen.press(KeyCode::Down);
    }
    screen.press(KeyCode::Enter);
}

fn remove(app: &str, hard: bool, serial: u64) -> Call {
    Call::Remove {
        app: app.into(),
        hard,
        consent: Consent::Explicit,
        serial,
    }
}

#[test]
fn opens_with_nothing_ticked_and_enter_alone_opens_nothing() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    let text = screen.text();
    assert!(!text.contains('◉'));
    assert!(text.contains(" 0 apps picked"));
    screen.press(KeyCode::Enter).settle();
    assert!(!screen.text().contains('╭'));
    assert!(!engine
        .calls()
        .iter()
        .any(|call| matches!(call, Call::Freeze { .. })));
}

#[test]
fn refused_apps_are_greyed_with_the_reason_and_cannot_be_ticked() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    assert!(line_with(&mut screen, "Docker").contains("   – Docker"));
    assert!(line_with(&mut screen, "Docker").contains("3d ago     brew cask"));
    // The refused name is drawn dim (the highlighted row is reversed instead).
    let buffer = screen.buffer();
    let y = buffer_y(&buffer, "Docker");
    assert_eq!(buffer[(5, y)].symbol(), "D");
    assert!(buffer[(5, y)].modifier.contains(Modifier::DIM));

    screen.press(KeyCode::Down).press(KeyCode::Down).settle();
    assert!(line_with(&mut screen, "– Keynote").contains("❯ – Keynote"));
    assert!(screen.text().contains(APPLE_REFUSAL));
    screen.press(KeyCode::Char(' ')).settle();
    screen.press(KeyCode::Enter).settle();
    let text = screen.text();
    assert!(text.contains(" 0 apps picked") && !text.contains('◉') && !text.contains('╭'));
    assert!(!engine.calls().contains(&Call::Plan("Keynote".into())));
}

#[test]
fn several_apps_may_be_ticked_and_the_foot_counts_what_is_removable() {
    let (mut screen, _) = open(FakeEngine::normal(), STANDARD);
    tick(&mut screen, true);
    let text = screen.text();
    assert!(text.contains("◉ AltTab") && text.contains("◉ Pixelmator Pro"));
    // 31 MB of AltTab (its kept Group Container is not counted) + 1,500 MB.
    assert!(text.contains(" 1.5 GB removable in 2 apps picked"));
    screen.press(KeyCode::Char(' ')).settle();
    assert!(screen.text().contains(" 31 MB removable in 1 app picked"));
}

#[test]
fn enter_freezes_the_plans_then_opens_the_commands_menu() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    tick(&mut screen, false);
    screen.press(KeyCode::Enter).settle();
    let text = screen.text();
    assert!(text.contains("╭─ Uninstall 1 app · 31 MB "));
    let at = |label: &str| text.find(label).unwrap_or_else(|| panic!("{label}"));
    assert!(text.contains("│ ❯ Move to Trash  Put Back works in Finder"));
    assert!(at("Move to Trash") < at("Dry-run") && at("Dry-run") < at("Hard delete"));
    assert!(text.contains("Hard delete    skips the Trash, cannot be undone"));
    // The plan is built again and frozen before the menu shows; nothing removed.
    assert_eq!(
        engine.calls(),
        [
            Call::List,
            Call::Plan("AltTab".into()),
            Call::Plan("AltTab".into()),
            Call::Freeze {
                app: "AltTab".into(),
                serial: 1
            },
        ]
    );
    screen.press(KeyCode::Esc);
    assert!(!screen.text().contains('╭'));
    assert!(engine.removals().is_empty());
}

#[test]
fn hard_delete_asks_with_cancel_focused_and_cancel_changes_nothing() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    tick(&mut screen, false);
    choose(&mut screen, 2);
    let text = screen.text();
    for fact in [
        "│ ● Delete 1 app for good?",
        "│   31 MB · 6 items",
        "│   AltTab",
        "│   It skips the Trash. Nothing can restore it.",
        "│  Cancel     Delete for good",
    ] {
        assert!(text.contains(fact), "{fact}\n{text}");
    }
    let buffer = screen.buffer();
    let row = buffer_row(&buffer, "Delete for good");
    let cancel = row.find("Cancel").unwrap();
    let x = u16::try_from(row[..cancel].chars().count()).unwrap();
    let y = buffer_y(&buffer, "Delete for good");
    assert!(buffer[(x, y)].modifier.contains(Modifier::REVERSED));

    screen.press(KeyCode::Enter).settle();
    assert!(!screen.text().contains('╭'));
    choose(&mut screen, 2);
    screen.press(KeyCode::Esc).settle();
    assert!(!screen.text().contains('╭'));
    assert!(engine.removals().is_empty());
    assert!(screen.text().contains("◉ AltTab"), "picks survive a cancel");
}

fn buffer_row(buffer: &ratatui::buffer::Buffer, needle: &str) -> String {
    crate::tui::page_harness::buffer_text(buffer)
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_default()
        .to_string()
}

fn buffer_y(buffer: &ratatui::buffer::Buffer, needle: &str) -> u16 {
    let text = crate::tui::page_harness::buffer_text(buffer);
    let index = text.lines().position(|line| line.contains(needle)).unwrap();
    u16::try_from(index).unwrap()
}

#[test]
fn hard_delete_confirmed_removes_the_frozen_plan_for_good_with_the_menus_consent() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    tick(&mut screen, false);
    choose(&mut screen, 2);
    screen.press(KeyCode::Right).press(KeyCode::Enter).settle();
    assert_eq!(engine.removals(), [remove("AltTab", true, 1)]);
    let text = screen.text();
    assert!(text.contains("✓ AltTab") && text.contains("For good"));
}

#[test]
fn move_to_trash_removes_each_picked_app_with_the_menus_consent() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    tick(&mut screen, true);
    choose(&mut screen, 0);
    screen.settle();
    assert_eq!(
        engine.removals(),
        [
            remove("AltTab", false, 1),
            remove("Pixelmator Pro", false, 2)
        ]
    );
    let text = screen.text();
    assert!(text.contains("│ Done"), "{text}");
    assert!(line_with(&mut screen, "✓ AltTab").contains("31 MB"));
    assert!(line_with(&mut screen, "✓ Pixelmator Pro").contains("1.5 GB"));
    assert!(text.contains("Removed    1.5 GB"));
    assert!(text.contains("enter back to Uninstall, read again"));

    screen.press(KeyCode::Enter).settle();
    let lists = engine.calls().iter().filter(|c| **c == Call::List).count();
    assert_eq!(lists, 2, "enter reads the list again");
    assert!(screen.text().contains(" 0 apps picked"));
}

#[test]
fn a_running_app_is_refused_with_the_engines_words() {
    let engine = FakeEngine::normal().failing("AltTab", "AltTab is running");
    let (mut screen, _) = open(engine, WIDE);
    tick(&mut screen, false);
    choose(&mut screen, 0);
    screen.settle();
    assert!(line_with(&mut screen, "● AltTab").contains("kept  AltTab is running"));
    assert!(screen.text().contains("Kept       1 app"));
}

#[test]
fn a_refusal_found_at_enter_removes_nothing() {
    let engine = FakeEngine::normal()
        .refusing_later("Pixelmator Pro", "Pixelmator Pro is a Homebrew cask now");
    let (mut screen, engine) = open(engine, WIDE);
    tick(&mut screen, true);
    screen.press(KeyCode::Enter).settle();
    let text = screen.text();
    assert!(!text.contains('╭'), "no menu opens");
    assert!(text.contains("│ Nothing removed"));
    let text = screen.text();
    assert!(
        line_with(&mut screen, "● Pixelmator Pro").contains("kept  Pixelmator Pro is a Ho"),
        "{text}"
    );
    assert!(line_with(&mut screen, "AltTab").contains("not run"));
    assert!(engine.removals().is_empty());
}

#[test]
fn dry_run_lists_what_would_go_and_changes_nothing() {
    let (mut screen, engine) = open(FakeEngine::normal(), WIDE);
    tick(&mut screen, false);
    choose(&mut screen, 1);
    let text = screen.text();
    for entry in [
        "│ Dry-run",
        "Would go   31 MB",
        "app      11 MB /Applications/AltTab.app",
        "cache    17 MB Caches/com.lwouis.alt-tab-macos",
        "data     12 KB Preferences/com.lwouis.alt-tab-macos.plist",
    ] {
        assert!(text.contains(entry), "{entry}\n{text}");
    }
    assert!(!text.contains("Group Containers"));
    screen.press(KeyCode::Enter).settle();
    assert!(screen.text().contains(" 31 MB removable in 1 app picked"));
    assert!(engine.removals().is_empty());
}

#[test]
fn quitting_during_a_run_waits_for_the_app_in_progress() {
    for quit in [
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        let (engine, gate) = FakeEngine::normal().gated_removals();
        let (mut screen, engine) = open(engine, STANDARD);
        tick(&mut screen, true);
        choose(&mut screen, 0);
        assert_eq!(gate.wait_entered(), "AltTab");
        screen.press_key(quit);
        assert!(!screen.exited(), "the app in progress is never cut off");
        gate.release(1);
        screen.settle();
        assert!(screen.exited());
        assert_eq!(engine.removals(), [remove("AltTab", false, 1)]);
    }
}

#[test]
fn esc_during_a_run_stops_after_the_app_in_progress() {
    let (engine, gate) = FakeEngine::normal().gated_removals();
    let (mut screen, engine) = open(engine, WIDE);
    tick(&mut screen, true);
    choose(&mut screen, 0);
    assert_eq!(gate.wait_entered(), "AltTab");
    screen.press(KeyCode::Esc);
    gate.release(1);
    screen.settle();
    assert!(!screen.exited());
    assert_eq!(engine.removals(), [remove("AltTab", false, 1)]);
    let text = screen.text();
    assert!(text.contains("│ Stopped"), "{text}");
    assert!(line_with(&mut screen, "Pixelmator Pro").contains("not run"));
}

#[test]
fn esc_stops_the_read_and_r_reads_again() {
    let mut pages = all_pages();
    pages[PageId::Uninstall.index()] = register(UninstallPage::new(Arc::new(FakeEngine::normal())));
    let mut screen = PageHarness::with_pages(pages, PageId::Clean, STANDARD, IconMode::Unicode);
    screen
        .press(KeyCode::Char('2'))
        .press(KeyCode::Esc)
        .settle();
    let text = screen.text();
    assert!(text.contains("● Reading /Applications stopped"), "{text}");
    assert!(text.contains("r read again"));
    screen.press(KeyCode::Char('r')).settle();
    assert!(screen.text().contains("❯ ◯ AltTab"));
}

#[test]
fn an_empty_or_unreadable_applications_folder_is_one_line() {
    let (mut screen, _) = open(FakeEngine::empty(), STANDARD);
    assert!(screen.text().contains(" ✓ No apps in /Applications"));
    let reason = "failed to read /Applications: Permission denied (os error 13)";
    let (mut screen, _) = open(FakeEngine::unreadable(reason), STANDARD);
    let text = screen.text();
    assert!(text.contains(" ● /Applications could not be read"));
    assert!(text.contains("Permission denied"));
    assert!(text.contains(" r read again · tab page · q quit"));
}

#[test]
fn s_steps_through_the_commands_three_orders_and_keeps_the_cursor_on_its_app() {
    let (mut screen, _) = open(FakeEngine::normal(), STANDARD);
    let first =
        |screen: &mut PageHarness| screen.text().lines().nth(3).unwrap_or_default().to_string();
    assert!(first(&mut screen).contains("AltTab"));
    screen.press(KeyCode::Char('s')).settle();
    assert!(first(&mut screen).contains("Docker"), "size");
    assert!(screen.text().contains("❯ ◯ AltTab"));
    screen.press(KeyCode::Char('s')).settle();
    assert!(first(&mut screen).contains("AltTab"), "name");
    screen.press(KeyCode::Down).settle();
    assert!(screen.text().contains("❯ – Docker"));
    screen.press(KeyCode::Char('s')).settle();
    assert!(first(&mut screen).contains("AltTab"), "last used");
    assert!(screen.text().contains("❯ – Docker"));
}
