//! What the boxes show before consent is given (S1), the list's words and
//! order (S10, C6), its columns with any name (C2), and one run at a time (S4).

use std::sync::Arc;

use crossterm::event::KeyCode;

use super::fake_engine::FakeEngine;
use super::UninstallPage;
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};
use crate::tui::pages::PageId;
use crate::tui::widgets::size_format::format_size;

const MB: u64 = 1_000_000;

fn open(engine: FakeEngine, size: (u16, u16)) -> PageHarness {
    PageHarness::new(
        UninstallPage::new(Arc::new(engine)),
        size,
        IconMode::Unicode,
    )
}

/// The rows of the box drawn over the page, between its borders.
fn box_rows(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let top = lines.iter().position(|l| l.contains('╭')).expect("a box");
    let bottom = lines.iter().position(|l| l.contains('╰')).expect("a box");
    lines[top + 1..bottom]
        .iter()
        .map(|l| l.chars().skip_while(|&c| c != '│').collect())
        .collect()
}

fn assert_split_shown(text: &str, app: u64, cache: u64, data: u64) {
    let rows = box_rows(text).join("\n");
    for part in [
        format!("app {}", format_size(app)),
        format!("cache {}", format_size(cache)),
        format!("data {}", format_size(data)),
        "● Removes your settings and data too:".to_string(),
    ] {
        assert!(rows.contains(&part), "{part:?} is not whole in\n{rows}");
    }
    assert!(rows.contains(&format_size(data)));
}

/// S1: the menu and the hard-delete box say how much of the total is the
/// app, its cache and the user's settings and data, from the frozen plans.
#[test]
fn both_boxes_split_the_total_into_app_cache_and_data() {
    let mut screen = open(FakeEngine::normal(), STANDARD);
    screen.press(KeyCode::Char(' ')).settle();
    screen.press(KeyCode::Down).settle();
    screen.press(KeyCode::Char(' ')).settle();
    screen.press(KeyCode::Enter).settle();
    // AltTab and Pixelmator Pro, as `FakeEngine::normal` plans them.
    let app = 11_310_000 + 1_240 * MB;
    let cache = 17_020_000 + 60 * MB;
    let data = 2_040_000 + 384_000 + 292_000 + 12_000 + 200 * MB;
    let menu = screen.text();
    assert!(menu.contains(&format!(
        "Uninstall 2 apps · {}",
        format_size(app + cache + data)
    )));
    assert_split_shown(&menu, app, cache, data);
    screen
        .press(KeyCode::Down)
        .press(KeyCode::Down)
        .press(KeyCode::Enter);
    let confirm = screen.text();
    assert!(confirm.contains("Delete 2 apps for good?"), "{confirm}");
    assert_split_shown(&confirm, app, cache, data);
}

/// S1: with the widest figures and names, every line stays whole inside
/// the box at 80×24.
#[test]
fn the_split_stays_inside_the_box_with_worst_case_figures() {
    let huge = u64::MAX / 3;
    let name = "Adobe Photoshop 2027 (Beta) – Creative Cloud Edition for Apple silicon";
    let mut screen = open(
        FakeEngine::listing(
            &[(name, huge, Some(1)), ("界面设计工具 🧑‍💻", huge, None)],
            huge,
            huge,
        ),
        STANDARD,
    );
    screen.press(KeyCode::Char(' ')).settle();
    screen.press(KeyCode::Down).settle();
    screen.press(KeyCode::Char(' ')).settle();
    screen.press(KeyCode::Enter).settle();
    let sum = huge.saturating_mul(2);
    assert_split_shown(&screen.text(), sum, sum, sum);
    screen
        .press(KeyCode::Down)
        .press(KeyCode::Down)
        .press(KeyCode::Enter);
    let confirm = screen.text();
    assert_split_shown(&confirm, sum, sum, sum);
    assert!(
        confirm.contains("It skips the Trash. Nothing can restore it."),
        "{confirm}"
    );
}

/// S10: no date from macOS reads `unknown`, as `sift uninstall` prints it.
#[test]
fn an_app_without_a_last_use_date_reads_unknown() {
    let mut screen = open(
        FakeEngine::listing(&[("Dated", MB, Some(12)), ("Undated", MB, None)], 0, 0),
        STANDARD,
    );
    let text = screen.text();
    let row = text.lines().find(|l| l.contains("Undated")).unwrap();
    assert!(row.contains("unknown"), "{row}");
    assert!(!text.contains("never"), "{text}");
}

/// C6: apps that tie in the chosen order are listed by name, every time.
#[test]
fn tied_apps_are_listed_by_name_then_path() {
    let names = [
        "Hotel", "golf", "Foxtrot", "echo", "Delta", "charlie", "Bravo", "alpha",
    ];
    let tied: Vec<(&str, u64, Option<u64>)> =
        names.iter().map(|&name| (name, 5 * MB, Some(30))).collect();
    let mut sorted = names.to_vec();
    sorted.sort_by_key(|name| name.to_lowercase());
    let order = |screen: &mut PageHarness| -> Vec<String> {
        let text = screen.text();
        text.lines()
            .filter_map(|line| names.iter().find(|n| line.contains(&format!(" {n} "))))
            .map(|n| n.to_string())
            .collect()
    };
    for _ in 0..3 {
        let mut screen = open(FakeEngine::listing(&tied, 0, 0), WIDE);
        assert_eq!(order(&mut screen), sorted, "last used ties");
        screen.press(KeyCode::Char('s'));
        assert_eq!(order(&mut screen), sorted, "size ties");
    }
}

/// C2: wide, joined and selector-carrying names never move a column or the pane divider.
#[test]
fn any_name_keeps_the_columns_and_the_divider_in_place() {
    let apps = [
        ("漢字のアプリケーション", 3 * MB, Some(4)),
        ("Notes \u{2764}\u{FE0F} pro", 2 * MB, Some(4)),
        ("Dev \u{1F469}\u{200D}\u{1F4BB} kit", MB, Some(4)),
        ("Plain", MB / 2, Some(4)),
    ];
    let mut screen = open(FakeEngine::listing(&apps, 0, 0), WIDE);
    let buffer = screen.buffer();
    let column =
        |y: u16, symbol: &str| (0..buffer.area.width).find(|&x| buffer[(x, y)].symbol() == symbol);
    let rows: Vec<u16> = (3..7).collect();
    let dividers: Vec<_> = rows.iter().map(|&y| column(y, "│")).collect();
    assert!(
        dividers.iter().all(|&x| x == Some(75)),
        "{dividers:?}\n{}",
        screen.text()
    );
    let ages: Vec<_> = rows
        .iter()
        .map(|&y| {
            (0..buffer.area.width)
                .find(|&x| buffer[(x, y)].symbol() == "4" && buffer[(x + 1, y)].symbol() == "d")
        })
        .collect();
    assert!(
        ages.windows(2).all(|w| w[0] == w[1]),
        "{ages:?}\n{}",
        screen.text()
    );
}

/// S4: during a run no key switches the page.
#[test]
fn a_run_keeps_the_page() {
    let (engine, gate) = FakeEngine::normal().gated_removals();
    let mut screen = open(engine, STANDARD);
    screen.press(KeyCode::Char(' ')).settle();
    screen.press(KeyCode::Enter).settle();
    screen.press(KeyCode::Enter);
    for key in [
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Char('1'),
        KeyCode::Char('3'),
    ] {
        screen.press(key);
        assert_eq!(screen.active(), PageId::Uninstall, "{key:?}");
    }
    gate.release(1);
    screen.settle();
    screen.press(KeyCode::Tab);
    assert_eq!(screen.active(), PageId::Scan);
}

/// A read opens on the first row that can be ticked, in the sorted order,
/// whatever order the engine listed the apps in; only `s` keeps the cursor
/// on its app.
#[test]
fn a_read_opens_on_the_first_tickable_row_as_sorted() {
    // Listed in this order; sorted by last use: Keynote, Alpha, Zed, Middle.
    let engine = FakeEngine::listing(
        &[
            ("Middle", MB, None),
            ("Zed", MB, Some(1)),
            ("Keynote", MB, Some(900)),
            ("Alpha", MB, Some(400)),
        ],
        0,
        0,
    )
    .refusing("Keynote", super::fake_engine::APPLE_REFUSAL);
    let mut screen = open(engine, STANDARD);
    let highlighted = |screen: &mut PageHarness| {
        let text = screen.text();
        text.lines()
            .find(|line| line.starts_with(" ❯ "))
            .unwrap_or_else(|| panic!("no highlighted row in\n{text}"))
            .to_string()
    };
    let text = screen.text();
    let first = text.lines().nth(3).unwrap_or_default();
    assert!(
        first.contains("– Keynote"),
        "the refused app sorts first:\n{text}"
    );
    assert!(highlighted(&mut screen).contains("◯ Alpha"));
    // `s` (size, then name) keeps the cursor on Alpha.
    screen.press(KeyCode::Down).press(KeyCode::Down).settle();
    assert!(highlighted(&mut screen).contains("Middle"));
    screen.press(KeyCode::Char('s')).settle();
    assert!(
        highlighted(&mut screen).contains("Middle"),
        "s keeps its app"
    );
    screen
        .press(KeyCode::Char('s'))
        .press(KeyCode::Char('s'))
        .settle();
    // `r` reads again: back to the first tickable row.
    screen.press(KeyCode::Char('r')).settle();
    assert!(
        highlighted(&mut screen).contains("◯ Alpha"),
        "{}",
        screen.text()
    );
}
