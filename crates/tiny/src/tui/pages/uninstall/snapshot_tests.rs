//! The page in the real frame against the approved mock's text dump
//! (`node tools/dump.js uninstall… | tail -n +3`), fed the mock's data by
//! the fake engine.

use std::sync::Arc;

use crossterm::event::KeyCode;

use super::fake_engine::FakeEngine;
use super::UninstallPage;
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};
use crate::tui::pages::{all_pages, register, PageId};

fn fixture(text: &str) -> String {
    text.trim_end_matches('\n').to_string()
}

fn opened(engine: FakeEngine, size: (u16, u16)) -> String {
    PageHarness::new(
        UninstallPage::new(Arc::new(engine)),
        size,
        IconMode::Unicode,
    )
    .text()
}

/// The worst-data dump with the two differences the harness and the engine
/// make: the harness reads the normal free space, and the long-named app is
/// listed with a date (see `FakeEngine::worst`) where the mock says `never`.
fn worst_fixture(text: &str, normal: &str) -> String {
    let tab_bar = normal.lines().next().unwrap_or_default();
    fixture(text)
        .lines()
        .enumerate()
        .map(|(index, line)| match index {
            0 => tab_bar.to_string(),
            _ => {
                // Padded before the right pane; the line's end otherwise.
                let line = line.replacen("never     ", "4,000d ago", 1);
                match line.strip_suffix("  never") {
                    Some(head) => format!("{head}  4,000d ago"),
                    None => line,
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_list_matches_the_mock_at_80_by_24() {
    assert_eq!(
        opened(FakeEngine::normal(), STANDARD),
        fixture(include_str!("fixtures/mock_std.txt"))
    );
}

#[test]
fn the_list_matches_the_mock_at_120_by_34() {
    assert_eq!(
        opened(FakeEngine::normal(), WIDE),
        fixture(include_str!("fixtures/mock_wide.txt"))
    );
}

#[test]
fn reading_matches_the_mock() {
    let mut pages = all_pages();
    pages[PageId::Uninstall.index()] = register(UninstallPage::new(Arc::new(FakeEngine::normal())));
    let mut screen = PageHarness::with_pages(pages, PageId::Clean, STANDARD, IconMode::Unicode);
    // The tab reads when it first shows; the answer is not handled yet.
    screen.press(KeyCode::Char('2'));
    assert_eq!(
        screen.text(),
        fixture(include_str!("fixtures/mock_reading_std.txt"))
    );
    screen.settle();
    assert!(screen.text().contains("❯ ◯ AltTab"));
}

#[test]
fn worst_data_matches_the_mock_at_80_by_24() {
    let normal = include_str!("fixtures/mock_std.txt");
    assert_eq!(
        opened(FakeEngine::worst(), STANDARD),
        worst_fixture(include_str!("fixtures/mock_worst_std.txt"), normal)
    );
}

#[test]
fn worst_data_matches_the_mock_at_120_by_34() {
    let normal = include_str!("fixtures/mock_wide.txt");
    assert_eq!(
        opened(FakeEngine::worst(), WIDE),
        worst_fixture(include_str!("fixtures/mock_worst_wide.txt"), normal)
    );
}
