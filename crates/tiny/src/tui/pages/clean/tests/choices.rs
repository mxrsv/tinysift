//! What the user chose survives a read again (S2), and the figure applies
//! the engine's rule for unticked paths (S7).

use crossterm::event::KeyCode;
use tiny_core::clean::types::{ComesBack, RiskLevel};

use super::super::engine::Route;
use super::super::fake_data::{found, home, normal};
use super::super::fake_engine::FakeEngine;
use super::{open, Session};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::WIDE;
use crate::tui::widgets::size_format::format_size;

fn foot(session: &mut Session) -> String {
    let text = session.text();
    text.lines()
        .find(|line| line.contains("picked in"))
        .unwrap_or_else(|| panic!("no foot in\n{text}"))
        .trim()
        .to_string()
}

/// The list row naming `label`, left of the detail pane.
fn row(session: &mut Session, label: &str) -> String {
    let text = session.text();
    text.lines()
        .map(|line| line.split('│').next().unwrap_or(line).to_string())
        .find(|line| line.contains(label))
        .unwrap_or_else(|| panic!("no {label} in\n{text}"))
}

/// S2: `r` and `v` (twice) keep a category's tick and an unticked path;
/// a Review category seen for the first time is unticked; one that left
/// and came back starts again from the default.
#[test]
fn reading_again_keeps_what_the_user_chose() {
    let mut session = open(FakeEngine::with(normal()), WIDE, IconMode::Unicode);
    // Untick Crash reports; untick Xcode's largest path.
    session.keys(&[KeyCode::Down, KeyCode::Char(' '), KeyCode::Home]);
    session.keys(&[KeyCode::Char('p'), KeyCode::Char(' '), KeyCode::Esc]);
    let chosen = foot(&mut session);
    assert!(
        chosen.starts_with("3.2 GB picked in 2 categories"),
        "{chosen}"
    );

    session.keys(&[KeyCode::Char('r')]);
    session.screen.settle();
    assert_eq!(foot(&mut session), chosen, "r keeps the choices");
    assert!(row(&mut session, "Crash reports").contains("◯ "));

    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    assert_eq!(foot(&mut session), chosen, "v adds Review rows, unticked");
    assert!(row(&mut session, "node_modules").contains("◯ "));
    // Tick node_modules, then leave Review and come back.
    let node = session
        .text()
        .lines()
        .skip(2)
        .position(|line| line.contains("node_modules"))
        .unwrap();
    session.keys(&[KeyCode::Home]);
    for _ in 0..node {
        session.keys(&[KeyCode::Down]);
    }
    session.keys(&[KeyCode::Char(' ')]);
    assert!(row(&mut session, "node_modules").contains("◉ "));
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    assert_eq!(
        foot(&mut session),
        chosen,
        "Review rows left with their ticks"
    );
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    assert_eq!(foot(&mut session), chosen);
    assert!(
        row(&mut session, "node_modules").contains("◯ "),
        "seen again: default"
    );

    // What enter hands over is still the choice.
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session.screen.settle();
    let plans = session.engine.plans.lock().unwrap();
    let ids: Vec<&str> = plans[0].groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["xcode-derived", "user-logs"]);
    let unticked = home("Library/Developer/Xcode/DerivedData/TinySift-fmzkqpelrbdvxn");
    assert_eq!(
        plans[0].excluded,
        std::collections::HashSet::from([std::path::PathBuf::from(unticked)])
    );
}

/// S7: a path unticked in one category keeps the item of another category
/// that holds it, as the engine does; the foot is what the run cleans.
#[test]
fn an_unticked_path_inside_another_categorys_item_keeps_that_item() {
    const MB: u64 = 1_000_000;
    let category = |id: &str, label: &str, items: Vec<(String, u64)>| {
        found(
            id,
            label,
            RiskLevel::Safe,
            Route::Trash,
            ComesBack::AppRecreates,
            "fixture",
            items,
        )
    };
    let outer = category(
        "user-caches",
        "User caches",
        vec![(home("x/cache"), 100 * MB), (home("y"), 10 * MB)],
    );
    let inner = category(
        "user-logs",
        "User logs",
        vec![(home("x/cache/sub/file"), 5 * MB), (home("z"), MB)],
    );
    let mut session = open(
        FakeEngine::with(vec![outer, inner]),
        WIDE,
        IconMode::Unicode,
    );
    assert!(foot(&mut session).starts_with("116 MB picked in 2 categories"));
    // Untick the file inside User caches' folder, from User logs' paths.
    session.keys(&[
        KeyCode::Down,
        KeyCode::Char('p'),
        KeyCode::Char(' '),
        KeyCode::Esc,
    ]);
    let figure = format_size(11 * MB);
    let shown = foot(&mut session);
    assert!(
        shown.starts_with(&format!("{figure} picked in 2 categories")),
        "{shown}"
    );
    session.keys(&[KeyCode::Enter]);
    assert!(session
        .text()
        .contains(&format!("Clean 2 categories · {figure}")));
    session.keys(&[KeyCode::Enter]);
    session.screen.settle();
    let done = session.text();
    assert!(done.contains(&format!("Cleaned    {figure}")), "{done}");
}
