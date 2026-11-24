//! What the page asks of the engine and what it hands to it.

use crossterm::event::KeyCode;
use tiny_core::clean::types::{CleanAction, ComesBack, RiskLevel};

use super::super::engine::{clean_options, Finding, Route};
use super::super::fake_data::{found, home, normal, not_found, worst};
use super::super::fake_engine::FakeEngine;
use super::super::list_view;
use super::super::listing::Listing;
use super::{open, Session};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{STANDARD, WIDE};
use crate::tui::widgets::size_format::format_size;

fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in\n{text}"))
}

/// The list row naming `needle` (left of the detail pane) and its index in the list.
fn list_row<'a>(text: &'a str, needle: &str) -> (usize, &'a str) {
    text.lines()
        .skip(2)
        .map(|line| line.split('│').next().unwrap_or(line))
        .enumerate()
        .find(|(_, row)| row.contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle:?} in\n{text}"))
}

/// TU8, I11: the figure on the foot and on the action box is exactly the
/// sum of the scan sizes of what enter hands to the engine.
#[test]
fn the_foot_and_the_box_title_are_the_sum_handed_to_the_engine() {
    let mut session = open(FakeEngine::with(normal()), WIDE, IconMode::Unicode);
    session.review_with_mock_picks();
    // Untick two node_modules paths: they are handed over as excluded paths.
    session.keys(&[
        KeyCode::Down,
        KeyCode::Char('p'),
        KeyCode::Char(' '),
        KeyCode::Down,
    ]);
    session.keys(&[KeyCode::Char(' '), KeyCode::Esc]);
    let foot = line_with(&session.text(), "picked in").to_string();
    session.keys(&[KeyCode::Enter]);
    let title = line_with(&session.text(), "╭─ Clean").to_string();
    session.keys(&[KeyCode::Enter]);
    session.screen.settle();

    let plans = session.engine.plans.lock().unwrap();
    let plan = &plans[0];
    assert_eq!(plan.action, CleanAction::Trash);
    let ids: Vec<&str> = plan.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "xcode-derived",
            "node-modules",
            "pnpm",
            "crash-reports",
            "user-logs"
        ]
    );
    let excluded: std::collections::HashSet<_> = [
        home("Code/old-landing/node_modules"),
        home("Code/client-portal-2025/node_modules"),
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .collect();
    assert_eq!(plan.excluded, excluded);
    // Summed here from what the engine received, not from the page's own figure.
    let handed: u64 = plan
        .groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|i| !plan.excluded.contains(&i.path))
        .map(|i| i.size)
        .sum();
    assert_eq!(handed, 10_929_050_000 - 820_000_000 - 610_000_000);
    let figure = format_size(handed);
    assert!(
        foot.contains(&format!("{figure} picked in 5 categories")),
        "{foot}"
    );
    assert!(
        title.contains(&format!("Clean 5 categories · {figure}")),
        "{title}"
    );
}

#[test]
fn the_figure_is_the_handed_sum_for_every_selection() {
    let discovery = || super::super::engine::Discovery {
        categories: normal(),
        ..Default::default()
    };
    for picks in 0u32..128 {
        let mut listing = Listing::new(discovery(), None);
        for (index, entry) in listing.entries.iter_mut().enumerate() {
            entry.ticked = picks & (1 << index) != 0;
        }
        let plan = listing.plan(CleanAction::Trash);
        let handed: u64 = plan.handed().map(|i| i.size).sum();
        let foot = list_view::foot(&listing, 200).text();
        assert!(
            foot.starts_with(&format!("{} picked", format_size(handed))),
            "{foot}"
        );
        // At the 60-column minimum the words go, the Trash and for-good figures stay.
        let narrow = list_view::foot(&listing, 59).text();
        assert!(narrow.chars().count() <= 59, "{narrow}");
        for figure in foot.split("● ").skip(1) {
            assert!(narrow.contains(figure.trim()), "{narrow} lost {figure}");
        }
    }
}

/// TU8, TU10: the options are `sift clean`'s own, Review only with `v`, never Destructive.
#[test]
fn the_tui_reads_what_sift_clean_lists() {
    use clap::Parser;
    #[derive(Parser)]
    struct Command {
        #[command(flatten)]
        opts: crate::cli::CleanOpts,
    }
    let cli: tiny_core::options::CleanOptions = (&Command::parse_from(["sift clean"]).opts).into();
    assert_eq!(clean_options(false), cli);
    let review = clean_options(true);
    assert!(review.include_review);
    for opts in [clean_options(false), review] {
        assert!(!opts.include_destructive && opts.category.is_empty() && !opts.include_trash);
    }
}

#[test]
fn review_rows_start_unticked_and_v_returns_to_safe_only() {
    let mut categories = normal();
    categories.push(found(
        "trash",
        "Emptied Trash fixture",
        RiskLevel::Destructive,
        Route::ReportOnly,
        ComesBack::NotRecoverable,
        "fixture",
        vec![(home(".Trash"), 1)],
    ));
    let mut session = open(FakeEngine::with(categories), STANDARD, IconMode::Unicode);
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    let text = session.text();
    for review in [
        "node_modules (idle)",
        "npm cache",
        "pnpm store",
        "Homebrew cleanup",
    ] {
        assert!(line_with(&text, review).contains('◯'), "{review} is ticked");
    }
    assert!(
        !text.contains("Emptied Trash fixture"),
        "a Destructive row was listed"
    );
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    assert_eq!(*session.engine.reads.lock().unwrap(), [false, true, false]);
    assert!(!session.text().contains("npm cache"));
}

/// I12: report-only rows and rows whose app runs are listed but never ticked.
#[test]
fn report_only_and_running_categories_cannot_be_ticked() {
    let mut session = open(FakeEngine::with(worst()), WIDE, IconMode::Unicode);
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    let before = line_with(&session.text(), "picked in").to_string();
    for name in ["Simulator runtime images", "Xcode DerivedData"] {
        let text = session.text();
        let (row, _) = list_row(&text, name);
        session.keys(&[KeyCode::Home]);
        session.keys(&vec![KeyCode::Down; row]);
        session.keys(&[KeyCode::Char(' ')]);
        let text = session.text();
        assert!(list_row(&text, name).1.contains("– "), "{name}");
        assert_eq!(line_with(&text, "picked in"), before);
    }
    let text = session.text();
    let (_, xcode) = list_row(&text, "Xcode DerivedData");
    assert!(
        xcode.contains("in use") && !xcode.contains(" B "),
        "{xcode}"
    );
    assert!(
        text.contains("not measured: Xcode is open"),
        "the detail names the app"
    );
}

#[test]
fn ticking_is_off_while_measuring() {
    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    session.keys(&[KeyCode::Char('r'), KeyCode::Char(' '), KeyCode::Enter]);
    let text = session.text();
    assert!(line_with(&text, "Xcode DerivedData").contains('◉'));
    assert!(!text.contains("Move to Trash"));
    session.screen.settle();
}

#[test]
fn esc_stops_measuring_and_lists_nothing() {
    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    session.keys(&[KeyCode::Char('r'), KeyCode::Esc]);
    session.screen.settle();
    let text = session.text();
    assert!(text.contains("Measuring stopped"));
    assert!(
        !text.contains("Xcode DerivedData"),
        "the stopped read is not listed"
    );
    session.keys(&[KeyCode::Char('r')]);
    session.screen.settle();
    assert!(session.text().contains("Xcode DerivedData"));
}

fn failed(id: &str, label: &str, risk: RiskLevel) -> super::super::engine::Category {
    not_found(
        id,
        label,
        risk,
        Finding::Failed {
            reason: "Full Disk Access is off".into(),
        },
    )
}

fn nothing_moves(session: &mut Session) {
    session.keys(&[
        KeyCode::Char(' '),
        KeyCode::Char('p'),
        KeyCode::Enter,
        KeyCode::Enter,
    ]);
    session.screen.settle();
    assert!(session.engine.plans.lock().unwrap().is_empty());
    assert!(!session.text().contains("Move to Trash"));
}

/// I13: a failed read turns cleaning off; a Review failure lifts with `v`, a Safe one only with `r`.
#[test]
fn a_failed_read_turns_cleaning_off_until_it_is_read_again() {
    let mut categories = normal();
    categories.push(failed(
        "mail-attachments",
        "Mail attachments",
        RiskLevel::Review,
    ));
    let mut session = open(FakeEngine::with(categories), STANDARD, IconMode::Unicode);
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    nothing_moves(&mut session);
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    session.keys(&[KeyCode::Enter]);
    assert!(
        session.text().contains("Move to Trash"),
        "Safe only cleans again"
    );

    let mut categories = normal();
    categories.push(failed("xcode-previews", "Xcode Previews", RiskLevel::Safe));
    let mut session = open(FakeEngine::with(categories), STANDARD, IconMode::Unicode);
    nothing_moves(&mut session);
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    assert_eq!(
        *session.engine.reads.lock().unwrap(),
        [false],
        "v is not offered"
    );
    session.engine.categories.lock().unwrap().pop();
    session.keys(&[KeyCode::Char('r')]);
    session.screen.settle();
    session.keys(&[KeyCode::Enter]);
    assert!(session.text().contains("Move to Trash"), "r read it again");
}

#[test]
fn dry_run_lists_the_plan_and_hands_nothing_over() {
    let mut session = open(FakeEngine::with(normal()), WIDE, IconMode::Unicode);
    session.keys(&[KeyCode::Enter, KeyCode::Down, KeyCode::Enter]);
    session.screen.settle();
    let text = session.text();
    assert!(text.contains("Dry-run"), "{text}");
    assert!(text.contains("TinySift-fmzkqpelrbdvxn"), "{text}");
    assert!(text.contains("6.3 GB in 3 categories"), "{text}");
    assert!(session.engine.plans.lock().unwrap().is_empty());
    session.keys(&[KeyCode::Esc]);
    assert!(session.text().contains("picked in 3 categories"));
}

/// TU9: Hard delete runs only from enter on the moved focus; enter on Cancel and esc change nothing.
#[test]
fn hard_delete_needs_its_confirmation() {
    let mut session = open(FakeEngine::with(normal()), STANDARD, IconMode::Unicode);
    let to_hard = [
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Enter,
    ];
    session.keys(&to_hard);
    let text = session.text();
    assert!(text.contains("Delete 3 categories for good?"));
    assert!(text.contains("6.3 GB · 141 items"), "{text}");
    session.keys(&[KeyCode::Enter]);
    session.keys(&to_hard).keys(&[KeyCode::Esc]);
    session.screen.settle();
    assert!(session.engine.plans.lock().unwrap().is_empty());
    session
        .keys(&to_hard)
        .keys(&[KeyCode::Right, KeyCode::Enter]);
    session.screen.settle();
    let plans = session.engine.plans.lock().unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].action, CleanAction::HardDelete);
}
