//! The Clean page against the fake engine: the mock's screens (`screens`),
//! what reaches the engine (`engine_calls`) and how a run ends (`runs`).

mod engine_calls;
mod runs;
mod screens;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;

use super::fake_engine::FakeEngine;
use super::CleanPage;
use crate::tui::icons::IconMode;
use crate::tui::page_harness::PageHarness;

/// The time as the page reads it, moved by the test.
#[derive(Clone)]
pub struct TestClock {
    base: Instant,
    millis: Arc<AtomicU64>,
}

impl TestClock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            millis: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set(&self, millis: u64) {
        self.millis.store(millis, Ordering::SeqCst);
    }
}

pub struct Session {
    pub screen: PageHarness,
    pub engine: Arc<FakeEngine>,
    pub clock: TestClock,
}

/// The Clean page on `engine`, opened (first read settled) at `size`.
pub fn open(engine: FakeEngine, size: (u16, u16), icons: IconMode) -> Session {
    let engine = Arc::new(engine);
    let clock = TestClock::new();
    let reader = clock.clone();
    let page = CleanPage::new(
        engine.clone(),
        Arc::new(move || reader.base + Duration::from_millis(reader.millis.load(Ordering::SeqCst))),
    );
    Session {
        screen: PageHarness::new(page, size, icons),
        engine,
        clock,
    }
}

impl Session {
    pub fn keys(&mut self, codes: &[KeyCode]) -> &mut Self {
        for &code in codes {
            self.screen.press(code);
        }
        self
    }

    /// After `v`: tick node_modules and pnpm store, the user's picks in the mock, and go back to the top.
    pub fn review_with_mock_picks(&mut self) -> &mut Self {
        self.keys(&[KeyCode::Char('v')]);
        self.screen.settle();
        self.keys(&[
            KeyCode::Down,
            KeyCode::Char(' '),
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Char(' '),
            KeyCode::Home,
        ])
    }

    pub fn text(&mut self) -> String {
        self.screen.text()
    }
}

/// One screen of a fixture: the `tools/dump.js` output for `size` and `icons`
/// without its `#####` line. `rows` lines are taken (the terminal's height,
/// or the exit card's length).
pub fn fixture(file: &str, size: (u16, u16), icons: IconMode, rows: usize) -> String {
    let size_name = if size.0 >= 100 { "wide" } else { "std" };
    let icons_name = match icons {
        IconMode::Unicode => "uni",
        IconMode::Nerd => "nerd",
    };
    let lines: Vec<&str> = file.lines().collect();
    let start = lines
        .iter()
        .position(|line| {
            line.starts_with("##### ")
                && line.contains(&format!("\"size\":\"{size_name}\""))
                && line.contains(&format!("\"icons\":\"{icons_name}\""))
        })
        .unwrap_or_else(|| panic!("no {size_name}/{icons_name} section"));
    lines[start + 1..start + 1 + rows].join("\n")
}

pub const SIZES: [(u16, u16); 2] = [
    crate::tui::page_harness::STANDARD,
    crate::tui::page_harness::WIDE,
];
pub const ICONS: [IconMode; 2] = [IconMode::Unicode, IconMode::Nerd];

/// Every size and icon set the mock draws.
pub fn every_layout() -> impl Iterator<Item = ((u16, u16), IconMode)> {
    SIZES
        .into_iter()
        .flat_map(|size| ICONS.into_iter().map(move |icons| (size, icons)))
}
