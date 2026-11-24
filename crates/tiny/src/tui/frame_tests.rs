//! The frame against the approved mock. `MockClean` is a test-only page that
//! draws the mock's Clean data through the shared widgets, so these tests pin
//! the frame (regions, widths, overlay placement) to `tools/dump.js` output.

use super::*;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};
use crate::tui::pages::{KeyHint, Page, PageView, ViewCtx};
use ratatui::style::{Color, Modifier};

#[path = "fixtures/mock_clean.rs"]
mod mock_clean;
use mock_clean::{Boxed, MockClean};

fn mock(review: bool, boxed: Boxed, size: (u16, u16), icons: IconMode) -> String {
    PageHarness::new(MockClean { review, boxed }, size, icons).text()
}

fn fixture(text: &str) -> String {
    text.trim_end_matches('\n').to_string()
}

#[test]
fn clean_matches_the_mock_at_80_by_24() {
    let expected = fixture(include_str!("fixtures/mock_clean_std.txt"));
    assert_eq!(
        mock(false, Boxed::None, STANDARD, IconMode::Unicode),
        expected
    );
}

#[test]
fn clean_matches_the_mock_at_120_by_34() {
    let expected = fixture(include_str!("fixtures/mock_clean_wide.txt"));
    assert_eq!(mock(false, Boxed::None, WIDE, IconMode::Unicode), expected);
}

#[test]
fn clean_matches_the_mock_in_nerd_mode() {
    let expected = fixture(include_str!("fixtures/mock_clean_wide_nerd.txt"));
    assert_eq!(mock(false, Boxed::None, WIDE, IconMode::Nerd), expected);
}

#[test]
fn the_action_box_lands_where_the_mock_draws_it() {
    let expected = fixture(include_str!("fixtures/mock_action_wide.txt"));
    assert_eq!(mock(true, Boxed::Action, WIDE, IconMode::Unicode), expected);
}

#[test]
fn the_hard_delete_box_lands_where_the_mock_draws_it() {
    let expected = fixture(include_str!("fixtures/mock_hard_std.txt"));
    assert_eq!(
        mock(true, Boxed::Hard, STANDARD, IconMode::Unicode),
        expected
    );
}

#[test]
fn the_empty_frame_at_both_sizes() {
    let mut std_screen = PageHarness::blank(STANDARD, IconMode::Unicode);
    let text = std_screen.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 24);
    assert_eq!(
        lines[0],
        "  Clean   Uninstall   Scan   Processes   System                    28.8 GB free"
    );
    assert_eq!(lines[1], "─".repeat(80));
    assert!(lines[2..22].iter().all(|l| l.is_empty()));
    assert_eq!(lines[22], "─".repeat(80));
    assert_eq!(lines[23], " tab page · q quit");

    let mut wide = PageHarness::blank(WIDE, IconMode::Nerd);
    let text = wide.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 34);
    assert!(
        lines[0].starts_with("  \u{F00E2} Clean   \u{F0A7A} Uninstall"),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with("28.8 GB free"));
}

#[test]
fn a_long_list_scrolls_and_keeps_the_cursor_in_view() {
    use crate::tui::pages::{KeyReply, PageCtx};
    use crate::tui::widgets::list_cursor::ListCursor;
    use crossterm::event::{KeyCode, KeyEvent};

    #[derive(Default)]
    struct Long {
        cursor: ListCursor,
    }
    impl Page for Long {
        const ID: PageId = PageId::Scan;
        type Msg = ();
        fn view(&self, _ctx: &ViewCtx) -> PageView {
            PageView {
                header: vec![StyledLine::new().dim("100 MB and up")],
                list: (0..85)
                    .map(|i| StyledLine::from(format!("row {i}")))
                    .collect(),
                cursor: Some(self.cursor.index()),
                keys: KeyHint::globals(),
                ..PageView::default()
            }
        }
        fn on_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, ()>) -> KeyReply {
            if self.cursor.on_key(&key, 85, ctx.list_rows(), |_| true) {
                KeyReply::Handled
            } else {
                KeyReply::Ignored
            }
        }
    }
    let mut screen = PageHarness::new(Long::default(), STANDARD, IconMode::Unicode);
    let lines = |screen: &mut PageHarness| -> Vec<String> {
        screen.text().lines().map(str::to_string).collect()
    };
    let first = lines(&mut screen);
    assert_eq!(first[2], " 100 MB and up");
    assert_eq!(first[3], " row 0");
    assert_eq!(first[21], "     … 67 more ↓");
    screen.press(KeyCode::PageDown);
    let paged = lines(&mut screen);
    assert_eq!(paged[2], " 100 MB and up", "the header stays pinned");
    assert_eq!(
        paged[20], " row 18",
        "the cursor row is the last one above the count"
    );
    assert_eq!(paged[21], "     … 66 more ↓");
    screen.press(KeyCode::End);
    let last = lines(&mut screen);
    assert_eq!(last[21], " row 84", "the last page has no count");
    assert_eq!(last[3], " row 66");
}

#[test]
fn colours_follow_roles_and_the_selection_is_reversed() {
    let mut screen = PageHarness::new(
        MockClean {
            review: false,
            boxed: Boxed::None,
        },
        WIDE,
        IconMode::Unicode,
    );
    let buffer = screen.buffer();
    // Row 3 is the second category, not selected: its bar starts at x = 40.
    assert_eq!(buffer[(40, 3)].fg, Color::Blue);
    assert_eq!(buffer[(1, 1)].fg, Color::DarkGray, "rules are faint");
    // The cursor row and the active tab are reversed without colour.
    assert!(buffer[(40, 2)].modifier.contains(Modifier::REVERSED));
    assert_eq!(buffer[(40, 2)].fg, Color::Reset);
    assert!(buffer[(2, 0)].modifier.contains(Modifier::REVERSED));
    for x in 0..120 {
        for y in 0..34 {
            assert_eq!(buffer[(x, y)].bg, Color::Reset, "no background at {x},{y}");
        }
    }

    let buffer = screen.monochrome().buffer();
    for x in 0..120 {
        for y in 0..34 {
            assert_eq!(
                buffer[(x, y)].fg,
                Color::Reset,
                "colour at {x},{y} with NO_COLOR"
            );
        }
    }
    assert!(buffer[(40, 2)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn rows_never_overflow_and_resizing_redraws_the_same_regions() {
    let mut screen = PageHarness::new(
        MockClean {
            review: true,
            boxed: Boxed::Hard,
        },
        WIDE,
        IconMode::Nerd,
    );
    for (width, height) in [(120, 34), (80, 24), (100, 30), (99, 30), (200, 60)] {
        screen.resize(width, height);
        let text = screen.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), usize::from(height));
        for line in &lines {
            assert!(
                text_width(line) <= usize::from(width),
                "{width}×{height}: {line}"
            );
        }
        assert_eq!(lines[1], "─".repeat(usize::from(width)));
        assert_eq!(
            lines[usize::from(height) - 2],
            "─".repeat(usize::from(width))
        );
        assert_eq!(
            lines[usize::from(height) - 1],
            " ←→ move · enter choose · esc cancel"
        );
    }
}

#[test]
fn the_spinner_turns_with_the_tick() {
    use crate::tui::widgets::spinner::spinner_glyph;
    struct Busy;
    impl Page for Busy {
        const ID: PageId = PageId::Scan;
        type Msg = ();
        fn view(&self, ctx: &ViewCtx) -> PageView {
            PageView {
                list: vec![StyledLine::new()
                    .storage(spinner_glyph(ctx.tick).to_string())
                    .dim(" reading… 48,210 files so far")],
                keys: vec![KeyHint::new("esc", "stop")],
                ..PageView::default()
            }
        }
        fn is_busy(&self) -> bool {
            true
        }
    }
    let mut screen = PageHarness::new(Busy, STANDARD, IconMode::Unicode);
    assert_eq!(
        screen.text().lines().nth(2),
        Some(" ⠹ reading… 48,210 files so far")
    );
    screen.set_tick(1);
    assert_eq!(
        screen.text().lines().nth(2),
        Some(" ⠸ reading… 48,210 files so far")
    );
}

#[test]
fn a_window_below_the_minimum_says_so_instead_of_drawing_the_frame() {
    let mut screen = PageHarness::blank(STANDARD, IconMode::Unicode);
    screen.resize(59, 20);
    let text = screen.text();
    assert!(text.contains("sift needs at least 60×18"));
    assert!(text.contains("this window is 59×20"));
    assert!(!text.contains("Uninstall"));
}

/// C1: a pinned row stays in view without being highlighted; a list with
/// neither a cursor nor a pin shows from its top, whatever the last offset.
#[test]
fn a_pinned_row_stays_in_view_and_a_list_without_one_starts_at_the_top() {
    let geo = Geometry::new(80, 24);
    let bar = TabBar {
        active: PageId::Clean,
        icons: IconMode::Unicode,
        free_bytes: None,
    };
    let view = |pin| PageView {
        list: (0..85)
            .map(|i| StyledLine::from(format!("row {i}")))
            .collect(),
        pin,
        ..PageView::default()
    };
    let shown = compose(&geo, &bar, &view(None), 40);
    assert_eq!(shown.offset, 0);
    assert_eq!(shown.rows[2].text().trim_end(), " row 0");
    let pinned = compose(&geo, &bar, &view(Some(60)), 0);
    let text: Vec<String> = pinned.rows.iter().map(StyledLine::text).collect();
    assert!(
        text.iter().any(|row| row.trim_end() == " row 60"),
        "{text:?}"
    );
    // Nothing in the body is drawn as highlighted (the tab bar's active tab is).
    let body = &pinned.rows[2..pinned.rows.len() - 2];
    assert!(body
        .iter()
        .all(|row| row.segs().iter().all(|seg| !seg.selected)));
}

/// The smallest window the TUI opens in (I7).
const SMALLEST: (u16, u16) = (60, 18);

/// A Clean mock screen: review on, which box, and a key pressed first.
type MockScreen = (bool, fn() -> Boxed, Option<crossterm::event::KeyCode>);

/// The rows of the box drawn over `text` that hold something, without its borders.
fn box_content(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let top = lines.iter().position(|line| line.contains('╭'));
    let bottom = lines.iter().position(|line| line.contains('╰'));
    let (Some(top), Some(bottom)) = (top, bottom) else {
        return Vec::new();
    };
    lines[top + 1..bottom]
        .iter()
        .filter_map(|line| {
            let inner = line.split('│').nth(1)?.trim();
            (!inner.is_empty()).then(|| inner.to_string())
        })
        .collect()
}

#[test]
fn the_smallest_window_keeps_every_box_whole_and_the_way_out() {
    use crossterm::event::KeyCode;
    let (width, height) = SMALLEST;
    let screens: [MockScreen; 5] = [
        (false, || Boxed::None, None),
        (true, || Boxed::None, None),
        (true, || Boxed::Action, None),
        (true, || Boxed::Hard, None),
        (true, || Boxed::None, Some(KeyCode::Char('?'))),
    ];
    for (review, boxed, key) in screens {
        let shot = |size| {
            let page = MockClean {
                review,
                boxed: boxed(),
            };
            let mut screen = PageHarness::new(page, size, IconMode::Unicode);
            if let Some(key) = key {
                screen.press(key);
            }
            screen.text()
        };
        let small = shot(SMALLEST);
        let lines: Vec<&str> = small.lines().collect();
        assert_eq!(lines.len(), usize::from(height));
        for line in &lines {
            assert!(text_width(line) <= usize::from(width), "{line}");
        }
        let key_bar = lines[usize::from(height) - 1];
        assert!(
            ["q quit", "esc cancel", "esc close"]
                .iter()
                .any(|way_out| key_bar.ends_with(way_out)),
            "{key_bar}"
        );
        assert!(
            small.contains('╭') == small.contains('╰'),
            "a box lost its bottom border:\n{small}"
        );
        assert_eq!(
            box_content(&small),
            box_content(&shot(STANDARD)),
            "a box lost or cut a row:\n{small}"
        );
    }
}

#[test]
fn a_narrow_key_bar_keeps_quit_and_points_to_the_full_list() {
    let keys = [
        KeyHint::new("↑↓", "move"),
        KeyHint::new("space", "tick"),
        KeyHint::new("v", "review"),
        KeyHint::new("p", "paths"),
        KeyHint::new("enter", "clean"),
        KeyHint::new("tab", "page"),
        KeyHint::new("q", "quit"),
    ];
    assert_eq!(
        key_bar(&keys, 60).text(),
        " space tick · v review · enter clean · ? keys · q quit"
    );
    assert_eq!(
        key_bar(&keys, 44).text(),
        " space tick · enter clean · ? keys · q quit"
    );
    let boxed = [
        KeyHint::new("←→", "move"),
        KeyHint::new("enter", "choose"),
        KeyHint::new("esc", "cancel"),
    ];
    assert_eq!(key_bar(&boxed, 30).text(), " enter choose · esc cancel");
    assert_eq!(
        key_bar(&boxed, 80).text(),
        " ←→ move · enter choose · esc cancel"
    );
}

#[test]
fn a_narrow_tab_bar_drops_the_free_figure_not_a_tab() {
    let bar = TabBar {
        active: PageId::Clean,
        icons: IconMode::Unicode,
        free_bytes: Some(MOCK_FREE),
    };
    let narrow = tab_bar(50, &bar).text();
    assert!(
        narrow.contains("System") && !narrow.contains("free"),
        "{narrow}"
    );
    assert!(tab_bar(80, &bar).text().contains("28.8 GB free"));
}

const MOCK_FREE: u64 = crate::tui::page_harness::MOCK_FREE_BYTES;
