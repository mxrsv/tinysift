//! Each Clean screen beside the approved mock's text dump
//! (`tools/dump.js <screen>,size=…,icons=…`), at 80×24 and 120×34, in
//! Unicode and Nerd mode, built from the mock's data through the fake engine.

use crossterm::event::KeyCode;

use super::super::engine::Finding;
use super::super::fake_data::{derived_data, home, node_modules, normal, not_found, worst};
use super::super::fake_engine::FakeEngine;
use super::super::Msg;
use super::{every_layout, fixture, open, Session};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::WIDE;
use crate::tui::pages::PageId;
use tiny_core::clean::execute::GroupOutcome;
use tiny_core::clean::types::{ExecReport, RiskLevel};

fn rows(size: (u16, u16)) -> usize {
    usize::from(size.1)
}

fn check(
    file: &str,
    build: impl Fn((u16, u16), IconMode) -> Session,
    edit: impl Fn(String) -> String,
) {
    for (size, icons) in every_layout() {
        let mut session = build(size, icons);
        let expected = edit(fixture(file, size, icons, rows(size)));
        assert_eq!(session.text(), expected, "{size:?} {icons:?}");
    }
}

fn same(text: String) -> String {
    text
}

#[test]
fn clean_opens_on_the_safe_categories_ticked() {
    check(
        include_str!("../../../fixtures/clean/clean.txt"),
        |size, icons| open(FakeEngine::with(normal()), size, icons),
        same,
    );
}

#[test]
fn review_lists_every_category_after_v() {
    check(
        include_str!("../../../fixtures/clean/review.txt"),
        |size, icons| {
            let mut session = open(FakeEngine::with(normal()), size, icons);
            session.review_with_mock_picks();
            session
        },
        same,
    );
}

#[test]
fn paths_list_every_path_of_the_highlighted_category() {
    check(
        include_str!("../../../fixtures/clean/paths.txt"),
        |size, icons| {
            let mut session = open(FakeEngine::with(normal()), size, icons);
            session.keys(&[KeyCode::Char('v')]);
            session.screen.settle();
            session.keys(&[KeyCode::Down, KeyCode::Char('p')]);
            session
        },
        same,
    );
}

fn picked(size: (u16, u16), icons: IconMode, then: &[KeyCode]) -> Session {
    let mut session = open(FakeEngine::with(normal()), size, icons);
    session.review_with_mock_picks().keys(then);
    session
}

#[test]
fn the_action_box_opens_on_enter() {
    check(
        include_str!("../../../fixtures/clean/action.txt"),
        |size, icons| picked(size, icons, &[KeyCode::Enter]),
        same,
    );
}

#[test]
fn hard_delete_opens_its_confirmation() {
    check(
        include_str!("../../../fixtures/clean/hard.txt"),
        |size, icons| {
            picked(
                size,
                icons,
                &[
                    KeyCode::Enter,
                    KeyCode::Down,
                    KeyCode::Down,
                    KeyCode::Down,
                    KeyCode::Enter,
                ],
            )
        },
        same,
    );
}

fn removed(category: &str, paths: Vec<std::path::PathBuf>) -> Msg {
    Msg::Group(GroupOutcome {
        category_id: category.into(),
        report: ExecReport {
            removed_paths: paths,
            ..ExecReport::default()
        },
    })
}

fn all_paths(category: tiny_core::clean::discover::CategoryGroup) -> Vec<std::path::PathBuf> {
    category.items.into_iter().map(|i| i.path).collect()
}

#[test]
fn running_shows_a_tick_per_finished_category_and_a_spinner_on_the_next() {
    let file = include_str!("../../../fixtures/clean/run.txt");
    let group = |category: super::super::engine::Category| match category.finding {
        Finding::Found(group) => group,
        _ => unreachable!("the mock's categories are found"),
    };
    for (size, icons) in every_layout() {
        let mut session = picked(size, icons, &[]);
        session.engine.hold();
        session.keys(&[KeyCode::Enter, KeyCode::Enter]);
        session
            .screen
            .deliver(
                PageId::Clean,
                removed("xcode-derived", all_paths(group(derived_data()))),
            )
            .deliver(
                PageId::Clean,
                removed("node-modules", all_paths(group(node_modules()))),
            )
            .set_tick(0);
        session.clock.set(4_100);
        let text = session.text();
        session.engine.release();
        session.screen.settle();
        // The engine names no tool command, so the running line says what
        // happens instead of the mock's `pnpm store prune…`.
        let expected = fixture(file, size, icons, rows(size))
            .replace("pnpm store prune… ", "removing for good…")
            .replace("pnpm store prune…", "removing for good…");
        assert_eq!(text, expected, "{size:?} {icons:?}");
    }
}

fn done(size: (u16, u16), icons: IconMode, engine: FakeEngine) -> Session {
    let mut session = open(engine, size, icons);
    session.review_with_mock_picks();
    session.keys(&[KeyCode::Enter, KeyCode::Enter]);
    session.clock.set(9_400);
    session.screen.settle();
    session
}

#[test]
fn done_says_cleaned_split_into_trash_and_for_good() {
    check(
        include_str!("../../../fixtures/clean/done.txt"),
        |size, icons| done(size, icons, FakeEngine::with(normal())),
        same,
    );
}

/// The mock's kept session: Xcode kept whole, one node_modules path kept.
pub fn keeping() -> FakeEngine {
    let engine = FakeEngine::with(normal());
    if let Finding::Found(group) = derived_data().finding {
        for item in group.items {
            engine.keep(&item.path.to_string_lossy(), "kept: Xcode is open");
        }
    }
    engine.keep(
        &home("Code/old-landing/node_modules"),
        "kept: changed after the scan",
    );
    engine
}

#[test]
fn done_shows_what_was_kept_and_why() {
    check(
        include_str!("../../../fixtures/clean/done_kept.txt"),
        |size, icons| done(size, icons, keeping()),
        same,
    );
}

#[test]
fn the_exit_card_is_the_mock_frame() {
    let file = include_str!("../../../fixtures/clean/exit.txt");
    for (engine, state) in [(FakeEngine::with(normal()), "ok"), (keeping(), "error")] {
        let session = done(WIDE, IconMode::Unicode, engine);
        let card: Vec<String> = session
            .screen
            .finish()
            .exit_card
            .iter()
            .map(|line| line.text().trim_end().to_string())
            .collect();
        let section = file
            .split("##### ")
            .find(|s| s.contains(&format!("\"state\":\"{state}\"")))
            .expect("exit section");
        // The dump draws the shell around the card: `% sift` above, the prompt below.
        let lines: Vec<&str> = section.lines().skip(2).collect();
        let lines: Vec<&str> = lines
            .iter()
            .copied()
            .take_while(|l| !l.starts_with("you@mac"))
            .collect();
        assert_eq!(card, lines, "{state}");
    }
}

#[test]
fn measuring_keeps_the_last_rows_and_turns_ticking_off() {
    check(
        include_str!("../../../fixtures/clean/scanning.txt"),
        |size, icons| {
            let mut session = open(FakeEngine::with(normal()), size, icons);
            session.keys(&[KeyCode::Char('r')]);
            session
                .screen
                .deliver(
                    PageId::Clean,
                    Msg::Progress {
                        read: 2,
                        done: 3,
                        total: 6,
                    },
                )
                .set_tick(0);
            session
        },
        same,
    );
}

#[test]
fn nothing_to_clean_in_either_view() {
    check(
        include_str!("../../../fixtures/clean/empty.txt"),
        |size, icons| open(FakeEngine::with(Vec::new()), size, icons),
        same,
    );
    check(
        include_str!("../../../fixtures/clean/review_empty.txt"),
        |size, icons| {
            let mut session = open(FakeEngine::with(Vec::new()), size, icons);
            session.keys(&[KeyCode::Char('v')]);
            session.screen.settle();
            session
        },
        same,
    );
}

fn failing(
    categories: Vec<super::super::engine::Category>,
    id: &str,
    label: &str,
    risk: RiskLevel,
) -> FakeEngine {
    let mut categories = categories;
    categories.push(not_found(
        id,
        label,
        risk,
        Finding::Failed {
            reason: "Full Disk Access is off".into(),
        },
    ));
    FakeEngine::with(categories)
}

#[test]
fn a_failed_safe_read_turns_cleaning_off_with_r_the_only_way_on() {
    // Unicode only: the mock draws the failed row with the folder glyph, the
    // TUI with the category's own (`xcode-previews` is Apple's).
    for size in super::SIZES {
        let mut session = open(
            failing(
                normal(),
                "xcode-previews",
                "Xcode Previews",
                RiskLevel::Safe,
            ),
            size,
            IconMode::Unicode,
        );
        let expected = fixture(
            include_str!("../../../fixtures/clean/safe_error.txt"),
            size,
            IconMode::Unicode,
            rows(size),
        );
        assert_eq!(session.text(), expected, "{size:?}");
    }
}

#[test]
fn a_failed_review_read_offers_v_back_to_safe_only() {
    let found: Vec<_> = normal().into_iter().take(3).collect();
    check(
        include_str!("../../../fixtures/clean/review_error.txt"),
        |size, icons| {
            let engine = failing(
                found.clone(),
                "mail-attachments",
                "Mail attachments",
                RiskLevel::Review,
            );
            let mut session = open(engine, size, icons);
            session.keys(&[KeyCode::Char('v')]);
            session.screen.settle();
            session
        },
        // The mock keeps the user's tick on node_modules; a fresh read starts
        // Review unticked (TU8), and a failed read cannot be ticked (I13).
        |text| {
            text.replacen("◉ 󰎙 node_modules", "◯ 󰎙 node_modules", 1)
                .replacen("◉ node_modules", "◯ node_modules", 1)
        },
    );
}

#[test]
fn worst_data_keeps_every_row_inside_the_frame() {
    let mut session = open(FakeEngine::with(worst()), WIDE, IconMode::Unicode);
    session.keys(&[KeyCode::Char('v')]);
    session.screen.settle();
    // Tick node_modules and the Docker images, the user's picks in the mock.
    let text = session.text();
    // Lines 0 and 1 are the tab bar and its rule.
    let row = |name: &str| text.lines().position(|l| l.contains(name)).unwrap() - 2;
    let node = row("node_modules (idle)");
    let docker = row("Docker images/build cache");
    session.keys(&vec![KeyCode::Down; node]);
    session.keys(&[KeyCode::Char(' ')]);
    session.keys(&vec![KeyCode::Down; docker - node]);
    session.keys(&[KeyCode::Char(' '), KeyCode::Home]);
    let text = session.text();
    let expected = fixture(
        include_str!("../../../fixtures/clean/review_worst.txt"),
        WIDE,
        IconMode::Unicode,
        rows(WIDE),
    );
    // Rows are sorted largest first (I22), so the not-measured Xcode row sits
    // after every measured one where the mock lists it third; the tab bar's
    // free space is the harness's. The rows themselves and every other line match.
    let body = |t: &str| {
        let mut rows: Vec<String> = t
            .lines()
            .skip(2)
            .take(29)
            .map(|l| {
                l.get(..l.find('│').unwrap_or(l.len()))
                    .unwrap_or(l)
                    .trim_end()
                    .replacen('❯', " ", 1)
            })
            .collect();
        rows.sort();
        rows
    };
    assert_eq!(body(&text), body(&expected));
    let tail = |t: &str| t.lines().skip(31).map(String::from).collect::<Vec<_>>();
    assert_eq!(tail(&text), tail(&expected));
    for line in text.lines() {
        assert!(crate::tui::styled_line::text_width(line) <= 120, "{line}");
    }
}
