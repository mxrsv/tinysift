//! System against the mock's `system` screen, fed the mock's data through a fake engine.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossterm::event::KeyCode;

use super::engine::{SystemEngine, SystemReading, Volume};
use super::SystemPage;
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};

const GIB: u64 = 1 << 30;
const GB: u64 = 1_000_000_000;

/// Returns one reading and counts the reads; the first `panics` reads panic.
struct FakeSystem {
    reading: SystemReading,
    reads: Arc<AtomicUsize>,
    panics: usize,
}

impl SystemEngine for FakeSystem {
    fn read(&self) -> SystemReading {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        assert!(read >= self.panics, "sampler broke");
        self.reading.clone()
    }
}

fn tenths(value: f64, unit: u64) -> u64 {
    (value * 10.0).round() as u64 * (unit / 10)
}

fn volume(name: &str, used: f64, total: f64) -> Volume {
    Volume {
        name: name.into(),
        used: tenths(used, GB),
        total: tenths(total, GB),
    }
}

/// `DATA.normal` in `tools/mock.js`.
fn normal() -> SystemReading {
    SystemReading {
        host: Some("Macbook-Pro.local".into()),
        uptime_seconds: Some(5 * 86_400 + 9 * 3_600 + 17 * 60),
        chip: "Apple M4 Pro".into(),
        cores: 12,
        memory_used: Some(tenths(19.2, GIB)),
        memory_total: 24 * GIB,
        volumes: vec![volume("Macintosh HD", 431.6, 460.4)],
    }
}

/// `DATA.worst` in `tools/mock.js`.
fn worst() -> SystemReading {
    let mut volumes = vec![
        volume("Macintosh HD", 1_998.9, 2_000.2),
        volume(
            "An external volume with a very long name (Backup 2019–2026)",
            7_811.2,
            8_000.0,
        ),
    ];
    volumes.extend((0..12).map(|i| volume(&format!("Vol {}", i + 1), f64::from(i) * 3.1, 64.0)));
    SystemReading {
        host: Some(
            "a-very-long-host-name-given-by-the-office-network.corp.example.internal".into(),
        ),
        uptime_seconds: Some(412 * 86_400 + 23 * 3_600 + 59 * 60),
        chip: "Apple M4 Ultra".into(),
        cores: 32,
        memory_used: Some(tenths(127.6, GIB)),
        memory_total: 128 * GIB,
        volumes,
    }
}

fn open(
    reading: SystemReading,
    size: (u16, u16),
    icons: IconMode,
) -> (PageHarness, Arc<AtomicUsize>) {
    open_failing(reading, size, icons, 0)
}

fn open_failing(
    reading: SystemReading,
    size: (u16, u16),
    icons: IconMode,
    panics: usize,
) -> (PageHarness, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let page = SystemPage::new(FakeSystem {
        reading,
        reads: Arc::clone(&reads),
        panics,
    });
    (PageHarness::new(page, size, icons), reads)
}

/// The mock's dump, except line 0 for worst data: the harness always reads
/// `28.8 GB free`, the worst data `0.3 GB free`.
fn assert_mock(actual: &str, fixture: &str, skip_tab_bar: bool) {
    let expected: Vec<&str> = fixture.trim_end_matches('\n').lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    assert_eq!(actual.len(), expected.len());
    let from = usize::from(skip_tab_bar);
    assert_eq!(actual[from..], expected[from..]);
}

#[test]
fn matches_the_mock_at_80_by_24() {
    let (mut screen, _) = open(normal(), STANDARD, IconMode::Unicode);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/system_std.txt"),
        false,
    );
}

#[test]
fn matches_the_mock_at_120_by_34_in_both_icon_modes() {
    let (mut screen, _) = open(normal(), WIDE, IconMode::Unicode);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/system_wide.txt"),
        false,
    );
    let (mut screen, _) = open(normal(), WIDE, IconMode::Nerd);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/system_wide_nerd.txt"),
        false,
    );
}

#[test]
fn worst_data_matches_the_mock_at_both_sizes() {
    let (mut screen, _) = open(worst(), STANDARD, IconMode::Unicode);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/system_worst_std.txt"),
        true,
    );
    let (mut screen, _) = open(worst(), WIDE, IconMode::Unicode);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/system_worst_wide.txt"),
        true,
    );
}

#[test]
fn reads_once_on_show_and_again_only_on_r() {
    let (mut screen, reads) = open(normal(), STANDARD, IconMode::Unicode);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    screen.press(KeyCode::Tab).press(KeyCode::BackTab).settle();
    let _ = screen.text();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "showing the tab again does not read"
    );
    screen.press(KeyCode::Char('r')).settle();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[test]
fn a_disk_above_92_percent_is_drawn_in_the_bad_role() {
    use ratatui::style::Color;
    let (mut screen, _) = open(normal(), WIDE, IconMode::Unicode);
    let buffer = screen.buffer();
    // Row 3 is memory at 80%: its own tone. Row 6 is the disk at 94%: red.
    assert_eq!(buffer[(29, 3)].fg, Color::Magenta);
    assert_eq!(buffer[(29, 6)].fg, Color::Red);
}

#[test]
fn a_failed_read_says_so_and_r_reads_again() {
    let (mut screen, reads) = open_failing(normal(), STANDARD, IconMode::Unicode, 1);
    let text = screen.text();
    assert!(text.contains("● The system could not be read"), "{text}");
    assert!(text.contains("sampler broke"));
    screen.press(KeyCode::Char('r')).settle();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert!(screen.text().contains("Macintosh HD"));
}
