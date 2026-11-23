//! How a run goes and ends: the stop point (I14), the done screen (TU11),
//! measuring again (I15) and the exit card (TU4).

use std::sync::mpsc;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tiny_core::clean::types::CleanAction;

use super::super::engine::Discovery;
use super::super::fake_data::normal;
use super::super::fake_engine::FakeEngine;
use super::super::listing::Listing;
use super::super::run::Run;
use super::super::{CleanPage, Stage};
use super::{open, Session};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{STANDARD, WIDE};
use crate::tui::pages::{FrameRequest, KeyReply, Page, PageCtx, PageEnv, PageId};
use crate::tui::work::Spawner;

fn started(engine: FakeEngine) -> Session {
    let mut session = open(engine, WIDE, IconMode::Unicode);
    session.engine.hold();
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session
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

/// I14, TU3: esc takes effect after the category in progress; the rest are listed as not run.
#[test]
fn esc_stops_after_the_category_in_progress() {
    let mut session = started(FakeEngine::with(normal()));
    session.keys(&[KeyCode::Esc]);
    assert!(session.text().contains("stopping after this category"));
    session.engine.release();
    session.screen.settle();
    let text = session.text();
    assert!(text.contains("✓ Xcode DerivedData"), "{text}");
    assert_eq!(text.matches("not run").count(), 2, "{text}");
    assert!(text.contains("Not run    2 categories"), "{text}");
}

/// I14, TU4: `q`, Ctrl-C and a signal wait for the category in progress, then quit and print the card.
#[test]
fn quitting_during_a_run_waits_for_the_category_then_prints_the_card() {
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    for way in 0..3 {
        let mut session = started(FakeEngine::with(normal()));
        match way {
            0 => drop(session.screen.press(KeyCode::Char('q'))),
            1 => drop(session.screen.press_key(ctrl_c)),
            _ => drop(session.screen.signal()),
        }
        assert!(!session.screen.exited(), "a run holds the exit");
        session.engine.release();
        session.screen.settle();
        assert!(session.screen.exited());
        let card = card(session);
        assert!(card.contains("Cleaned 6.2 GB"), "{card}");
        assert!(
            !card.contains("User logs"),
            "only the category in progress ran"
        );
    }
}

fn run_once(session: &mut Session) {
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session.screen.settle();
}

/// TU4: several cleans in one session add up; a session without one prints nothing.
#[test]
fn the_exit_card_adds_up_the_session_and_is_absent_without_a_clean() {
    let session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    assert!(card(session).is_empty());

    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    session.keys(&[KeyCode::Enter, KeyCode::Down, KeyCode::Enter, KeyCode::Esc]);
    assert!(card(session).is_empty(), "a dry run is not a clean");

    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    session.clock.set(0);
    run_once(&mut session);
    session.keys(&[KeyCode::Enter]);
    session.screen.settle();
    session.keys(&[KeyCode::Char(' ')]); // the second clean leaves Xcode out
    run_once(&mut session);
    let card = card(session);
    assert!(
        card.contains("Cleaned 6.4 GB"),
        "6.3 GB then 89 MB:\n{card}"
    );
    assert!(line_with_all(&card, &["Crash reports", "178 MB"]), "{card}");
    assert!(
        line_with_all(&card, &["Xcode DerivedData", "6.2 GB"]),
        "{card}"
    );
    assert!(card.contains("In Trash 6.4 GB"), "{card}");
}

/// TU11: the result says Cleaned, never Freed; In Trash plus For good is the total; Kept stands apart.
#[test]
fn the_result_says_cleaned_and_its_parts_add_up() {
    let mut session = open(super::screens::keeping(), WIDE, IconMode::Unicode);
    session.review_with_mock_picks();
    run_once(&mut session);
    let text = session.text();
    assert!(line_with_all(&text, &["Cleaned", "3.9 GB"]));
    assert!(line_with_all(&text, &["In Trash", "3.1 GB"]));
    assert!(line_with_all(&text, &["For good", "840 MB"]));
    assert!(line_with_all(&text, &["Kept", "7.0 GB"]));
    let card = card(session);
    for screen in [&text, &card] {
        assert!(!screen.to_lowercase().contains("freed"), "{screen}");
    }
}

fn line_with_all(text: &str, parts: &[&str]) -> bool {
    text.lines()
        .any(|line| parts.iter().all(|p| line.contains(p)))
}

#[test]
fn hard_delete_counts_everything_as_for_good() {
    let mut session = open(FakeEngine::with(normal()), WIDE, IconMode::Unicode);
    session.keys(&[
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Enter,
    ]);
    session.keys(&[KeyCode::Right, KeyCode::Enter]);
    session.screen.settle();
    let text = session.text();
    assert!(line_with_all(&text, &["In Trash", "0 B"]), "{text}");
    assert!(line_with_all(&text, &["For good", "6.3 GB"]), "{text}");
}

#[test]
fn an_engine_refusal_ends_the_run_with_its_reason_and_no_card() {
    let engine = FakeEngine::with(normal());
    *engine.refuse.lock().unwrap() =
        Some("cannot read the kept paths saved by the TinySift app".into());
    let mut session = open(engine, WIDE, IconMode::Unicode);
    run_once(&mut session);
    let text = session.text();
    assert!(
        text.contains("Stopped: cannot read the kept paths"),
        "{text}"
    );
    assert_eq!(text.matches("not run").count(), 3);
    assert!(card(session).is_empty());
}

/// I15: enter on Done asks the frame for a fresh Clean and a fresh free-disk figure.
#[test]
fn enter_on_done_measures_again_and_refreshes_the_free_space() {
    let engine = Arc::new(FakeEngine::with(normal()));
    let mut page = CleanPage::new(engine.clone(), Arc::new(std::time::Instant::now));
    let listing = Listing::new(Discovery {
        categories: normal(),
        ..Discovery::default()
    });
    let mut run = Run::new(
        &listing.plan(CleanAction::Trash),
        &listing,
        std::time::Instant::now(),
    );
    run.end(Ok(Default::default()), Default::default());
    page.listing = Some(listing);
    page.stage = Stage::Done(run);
    let (tx, _rx) = mpsc::channel();
    let mut env = PageEnv {
        page: PageId::Clean,
        spawner: Spawner::new(tx),
        requests: Vec::new(),
        list_rows: 20,
        icons: IconMode::Unicode,
    };
    let reply = page.on_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut PageCtx::new(&mut env),
    );
    assert_eq!(reply, KeyReply::Handled);
    assert_eq!(
        env.requests,
        [
            FrameRequest::RefreshFreeSpace,
            FrameRequest::Show(PageId::Clean),
            FrameRequest::Reload(PageId::Clean),
        ]
    );

    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    run_once(&mut session);
    session.keys(&[KeyCode::Enter]);
    session.screen.settle();
    assert_eq!(session.engine.reads.lock().unwrap().len(), 2);
    assert!(session.text().contains("picked in 3 categories"));
}
