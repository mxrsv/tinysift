//! How the engine's answers read once a run ends: a requested removal is
//! not "Cleaned" (S8), a category the engine stopped inside is not "not
//! run" (S9), a long run keeps the category in progress in view (C1), and
//! only the run's own task can end it (C4).

use crossterm::event::KeyCode;
use tiny_core::clean::discover::CategoryGroup;
use tiny_core::clean::execute::GroupOutcome;
use tiny_core::clean::types::{ComesBack, ExecReport, RiskLevel};

use super::super::engine::{Category, Finding, Route};
use super::super::fake_data::{found, home, normal};
use super::super::fake_engine::FakeEngine;
use super::super::Msg;
use super::{open, Session};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{STANDARD, WIDE};
use crate::tui::pages::PageId;

fn group(categories: &[Category], id: &str) -> CategoryGroup {
    categories
        .iter()
        .find(|c| c.id == id)
        .and_then(|c| match &c.finding {
            Finding::Found(group) => Some(group.clone()),
            _ => None,
        })
        .unwrap()
}

fn card(session: Session) -> String {
    session
        .screen
        .finish()
        .exit_card
        .iter()
        .map(|line| line.text().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn list_row(text: &str, needle: &str) -> String {
    text.lines()
        .map(|line| line.split('│').next().unwrap_or(line).to_string())
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no {needle} in\n{text}"))
}

/// S8: a removal a tool only accepted is shown and totalled apart.
#[test]
fn a_requested_removal_is_shown_apart_from_what_was_cleaned() {
    let engine = FakeEngine::with(normal());
    let crash = group(&normal(), "crash-reports");
    *engine.request.lock().unwrap() = crash.items.iter().map(|i| i.path.clone()).collect();
    let mut session = open(engine, WIDE, IconMode::Unicode);
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session.screen.settle();
    let text = session.text();
    assert!(text.contains("Cleaned    6.2 GB"), "{text}");
    assert!(
        text.contains("Requested  89 MB  a tool finishes it"),
        "{text}"
    );
    let row = list_row(&text, "Crash reports");
    assert!(row.contains("0 B  89 MB requested"), "{row}");
    let card = card(session);
    assert!(card.contains("Cleaned 6.2 GB"), "{card}");
    assert!(
        card.contains("Requested 89 MB · a tool finishes it"),
        "{card}"
    );
}

/// S9: the engine stopped inside a category: that row says so, with the
/// reason and that it may be partly done; only the rest did not run.
#[test]
fn a_category_the_engine_stopped_inside_is_not_reported_as_not_run() {
    let engine = FakeEngine::with(normal());
    *engine.stop_in.lock().unwrap() = Some(("crash-reports".into(), "the tool crashed".into()));
    let mut session = open(engine, WIDE, IconMode::Unicode);
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session.screen.settle();
    let text = session.text();
    let crash = list_row(&text, "Crash reports");
    assert!(crash.contains("● Crash reports"), "{crash}");
    assert!(crash.contains("stopped  the tool crashed"), "{crash}");
    assert!(list_row(&text, "User logs").contains("not run"));
    assert!(text.contains("● Stopped in Crash reports /"), "{text}");
    assert!(text.contains("partly done"), "{text}");
    assert_eq!(
        text.matches("the tool crashed").count(),
        2,
        "not repeated as the run's error"
    );
}

/// 30 Safe categories of one item each, largest first.
fn many() -> Vec<Category> {
    (0..30u64)
        .map(|i| {
            found(
                &format!("cat-{i:02}"),
                &format!("Category {i:02}"),
                RiskLevel::Safe,
                Route::Trash,
                ComesBack::AppRecreates,
                "fixture",
                vec![(home(&format!("c/{i}")), (100 - i) * 1_000_000)],
            )
        })
        .collect()
}

/// C1: a run that starts after the list scrolled shows its first rows, then
/// scrolls with the category in progress.
#[test]
fn a_long_run_shows_its_top_then_follows_the_category_in_progress() {
    let mut session = open(FakeEngine::with(many()), STANDARD, IconMode::Unicode);
    session.keys(&[KeyCode::End]);
    assert!(
        !session.text().contains("Category 00"),
        "the list scrolled down"
    );
    session.engine.hold();
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    let text = session.text();
    assert!(
        text.contains("Category 00"),
        "the run starts from its top:\n{text}"
    );
    assert!(text.contains("moving to Trash…"), "{text}");
    for i in 0..20u64 {
        session.screen.deliver(
            PageId::Clean,
            Msg::Group(GroupOutcome {
                category_id: format!("cat-{i:02}"),
                report: ExecReport {
                    removed_paths: vec![home(&format!("c/{i}")).into()],
                    ..ExecReport::default()
                },
                stopped: None,
            }),
        );
    }
    let text = session.text();
    let running = list_row(&text, "Category 20");
    assert!(running.contains("moving to Trash…"), "{text}");
    session.engine.release();
    session.screen.settle();
}

/// C4: a failure of a task the page no longer owns ends nothing.
#[test]
fn a_stale_task_failure_never_ends_a_run() {
    let mut session = open(FakeEngine::with(normal()), WIDE, IconMode::Unicode);
    let stale = session.screen.stale_task(PageId::Clean);
    session.engine.hold();
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session
        .screen
        .fail_task(PageId::Clean, stale, "an old read panicked");
    assert!(session.text().contains("moving to Trash…"), "still running");
    session.engine.release();
    session.screen.settle();
    let text = session.text();
    assert!(text.contains("Cleaned    6.3 GB"), "{text}");
    assert!(!text.contains("an old read panicked"), "{text}");
}

/// C2: wide, joined and selector-carrying names never move a column or the pane divider.
#[test]
fn any_category_name_keeps_the_columns_and_the_divider_in_place() {
    let names = [
        "漢字のキャッシュ",
        "Notes \u{2764}\u{FE0F} cache",
        "Dev \u{1F469}\u{200D}\u{1F4BB} builds",
        "Plain cache",
    ];
    let categories = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            found(
                &format!("cat-{i}"),
                name,
                RiskLevel::Safe,
                Route::Trash,
                ComesBack::AppRecreates,
                "fixture",
                vec![(home(&format!("c/{i}")), (9 - i as u64) * 1_000_000)],
            )
        })
        .collect();
    let mut session = open(FakeEngine::with(categories), WIDE, IconMode::Unicode);
    let buffer = session.screen.buffer();
    let find =
        |y: u16, symbol: &str| (0..buffer.area.width).find(|&x| buffer[(x, y)].symbol() == symbol);
    let dividers: Vec<_> = (2..6).map(|y| find(y, "│")).collect();
    assert!(
        dividers.iter().all(|&x| x == Some(75)),
        "{dividers:?}\n{}",
        session.text()
    );
    // The size column ends in the same cell: the `B` of `MB`.
    let sizes: Vec<_> = (2..6)
        .map(|y| (0..75).rev().find(|&x| buffer[(x, y)].symbol() == "B"))
        .collect();
    assert!(
        sizes.windows(2).all(|w| w[0] == w[1]),
        "{sizes:?}\n{}",
        session.text()
    );
}

/// S3: tab in the hard-delete box neither moves its focus nor leaves the page.
#[test]
fn tab_in_the_hard_delete_box_does_nothing() {
    let mut session = open(FakeEngine::with(normal()), WIDE, IconMode::Unicode);
    session.keys(&[KeyCode::Enter, KeyCode::Up, KeyCode::Up]);
    session.keys(&[KeyCode::End, KeyCode::Down, KeyCode::Down, KeyCode::Down]);
    session.keys(&[KeyCode::Enter]);
    assert!(session.text().contains("Delete 3 categories for good?"));
    session.keys(&[KeyCode::Tab, KeyCode::BackTab, KeyCode::Enter]);
    assert_eq!(session.screen.active(), PageId::Clean);
    session.screen.settle();
    assert!(
        session.engine.plans.lock().unwrap().is_empty(),
        "enter cancelled"
    );
}

/// S6: a second Ctrl-C while the category in progress finishes leaves at once.
#[test]
fn a_second_ctrl_c_during_a_run_leaves_at_once() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    session.engine.hold();
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session.screen.press_key(ctrl_c);
    let text = session.text();
    assert!(
        text.contains("● Quitting after the category in progress · ctrl-c again quits now"),
        "{text}"
    );
    session.screen.press_key(ctrl_c);
    assert_eq!(session.screen.flow(), crate::tui::app::Flow::Abandon);
    session.engine.release();
    let end = session.screen.finish();
    assert_eq!(end.abandoned, Some("category"));
}
