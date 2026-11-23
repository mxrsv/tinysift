//! Processes against the mock's `procs` and `quit` screens, fed the mock's
//! data through a fake engine that records every quit and never signals.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossterm::event::KeyCode;
use ratatui::style::Color;
use tiny_core::processes::{Refusal, TerminateKind, TerminateOutcome};

use super::engine::ProcessRow;
use super::fake_engine::{normal, worst, Calls, FakeProcesses};
use super::ProcessesPage;
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};

struct Probe {
    calls: Calls,
    reads: Arc<AtomicUsize>,
}

fn open_with(
    data: Vec<ProcessRow>,
    refused: Option<(u32, Refusal)>,
    outcome: TerminateOutcome,
    size: (u16, u16),
) -> (PageHarness, Probe) {
    let probe = Probe {
        calls: Calls::default(),
        reads: Arc::new(AtomicUsize::new(0)),
    };
    let page = ProcessesPage::new(FakeProcesses {
        rows: data,
        refused,
        outcome,
        calls: Arc::clone(&probe.calls),
        reads: Arc::clone(&probe.reads),
    });
    (PageHarness::new(page, size, IconMode::Unicode), probe)
}

fn open(size: (u16, u16)) -> (PageHarness, Probe) {
    open_with(normal(), None, TerminateOutcome::Exited, size)
}

/// The mock's dump with `edits` applied to it, each a known difference named where it is used.
fn assert_mock(actual: &str, fixture: &str, edits: &[(&str, &str)]) {
    let expected = edits.iter().fold(
        fixture.trim_end_matches('\n').to_string(),
        |text, (from, to)| {
            assert!(text.contains(from), "{from} is not in the fixture");
            text.replace(from, to)
        },
    );
    assert_eq!(actual, expected);
}

/// The harness reads `28.8 GB free`; the worst data `0.3 GB free`. The mock
/// prints a zero size as `0 MB`; the shared size format prints `0 B`.
const WORST_EDITS: [(&str, &str); 2] = [
    (" 0.3 GB free", "28.8 GB free"),
    ("      0 MB", "       0 B"),
];

#[test]
fn matches_the_mock_at_both_sizes_and_in_nerd_mode() {
    let (mut screen, _) = open(STANDARD);
    assert_mock(&screen.text(), include_str!("fixtures/procs_std.txt"), &[]);
    let (mut screen, _) = open(WIDE);
    assert_mock(&screen.text(), include_str!("fixtures/procs_wide.txt"), &[]);
    let page = ProcessesPage::new(FakeProcesses {
        rows: normal(),
        refused: None,
        outcome: TerminateOutcome::Exited,
        calls: Calls::default(),
        reads: Arc::default(),
    });
    let mut nerd = PageHarness::new(page, WIDE, IconMode::Nerd);
    assert_mock(
        &nerd.text(),
        include_str!("fixtures/procs_wide_nerd.txt"),
        &[],
    );
}

#[test]
fn worst_data_matches_the_mock_at_both_sizes() {
    let (mut screen, _) = open_with(worst(), None, TerminateOutcome::Exited, STANDARD);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/procs_worst_std.txt"),
        &WORST_EDITS,
    );
    let (mut screen, _) = open_with(worst(), None, TerminateOutcome::Exited, WIDE);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/procs_worst_wide.txt"),
        &WORST_EDITS,
    );
}

#[test]
fn reads_once_and_never_refreshes_by_itself() {
    let (mut screen, probe) = open(STANDARD);
    assert_eq!(probe.reads.load(Ordering::SeqCst), 1);
    for tick in 0..20 {
        screen.set_tick(tick);
        let _ = screen.text();
    }
    screen.press(KeyCode::Tab).press(KeyCode::BackTab).settle();
    let before = screen.text();
    assert_eq!(probe.reads.load(Ordering::SeqCst), 1, "no read without r");
    assert_eq!(screen.text(), before, "rows stay where they are");
    screen.press(KeyCode::Char('r')).settle();
    assert_eq!(probe.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn s_switches_to_memory_order_and_keeps_the_highlighted_process() {
    let (mut screen, _) = open(STANDARD);
    screen.press(KeyCode::Char('s'));
    let text = screen.text();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[3].ends_with("node (claude)"), "{}", lines[3]);
    assert!(lines[4].starts_with(" ❯  17703"), "{}", lines[4]);
    screen.press(KeyCode::Char('s'));
    assert!(screen
        .text()
        .lines()
        .nth(3)
        .unwrap()
        .starts_with(" ❯  17703"));
}

#[test]
fn x_opens_the_quit_box_on_cancel_and_esc_or_enter_send_nothing() {
    for (size, fixture) in [
        (STANDARD, include_str!("fixtures/quit_std.txt")),
        (WIDE, include_str!("fixtures/quit_wide.txt")),
    ] {
        let (mut screen, probe) = open(size);
        screen.press(KeyCode::Char('x'));
        assert_mock(&screen.text(), fixture, &[]);
        screen.press(KeyCode::Esc).settle();
        assert!(!screen.text().contains("Quit process"));
        screen
            .press(KeyCode::Char('x'))
            .press(KeyCode::Enter)
            .settle();
        assert!(!screen.text().contains("Quit process"));
        assert!(probe.calls.lock().unwrap().is_empty(), "nothing was sent");
        assert_eq!(probe.reads.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn only_enter_on_quit_signals_through_the_engine_then_reads_again() {
    let (mut screen, probe) = open(STANDARD);
    screen
        .press(KeyCode::Char('x'))
        .press(KeyCode::Right)
        .press(KeyCode::Enter)
        .settle();
    let calls = probe.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let (target, kind) = &calls[0];
    assert_eq!((target.pid, target.start_time), (17703, 1_700_017_703));
    assert_eq!(*kind, TerminateKind::Graceful);
    assert_eq!(
        probe.reads.load(Ordering::SeqCst),
        2,
        "the list is read again"
    );
    let text = screen.text();
    assert!(
        text.contains("✓ Google Chrome Helper (Renderer) (PID 17703) ended."),
        "{text}"
    );
}

#[test]
fn force_quit_uses_the_same_box_with_its_loss_in_red() {
    let (mut screen, probe) = open(WIDE);
    screen.press(KeyCode::Char('X'));
    let text = screen.text();
    assert!(text.contains("╭─ Force quit process"), "{text}");
    assert!(text.contains("│ ● Force quit Google Chrome Helper (Renderer)?"));
    assert!(text.contains("│   Unsaved work is lost."));
    assert!(text.contains("│  Cancel     Force quit"));
    let row = text
        .lines()
        .position(|line| line.contains("Unsaved work is lost."))
        .unwrap();
    let column = text
        .lines()
        .nth(row)
        .unwrap()
        .chars()
        .position(|c| c == 'U')
        .unwrap();
    let buffer = screen.buffer();
    assert_eq!(buffer[(column as u16, row as u16)].fg, Color::Red);
    screen.press(KeyCode::Esc).settle();
    assert!(probe.calls.lock().unwrap().is_empty());
    screen
        .press(KeyCode::Char('X'))
        .press(KeyCode::Tab)
        .press(KeyCode::Enter)
        .settle();
    let calls = probe.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, TerminateKind::Force);
}

#[test]
fn a_refused_process_opens_no_box_sends_nothing_and_shows_the_reason() {
    for size in [STANDARD, WIDE] {
        let (mut screen, probe) = open_with(
            normal(),
            Some((17703, Refusal::OtherUser)),
            TerminateOutcome::Exited,
            size,
        );
        for key in ['x', 'X'] {
            screen.press(KeyCode::Char(key)).settle();
            let text = screen.text();
            assert!(!text.contains("Quit process"), "{text}");
            assert!(
                text.contains("● Cannot quit: it belongs to another user"),
                "{text}"
            );
            assert!(text.ends_with("x quit · X force quit · tab page · q quit"));
        }
        assert!(probe.calls.lock().unwrap().is_empty());
        // The reason belongs to that process: the next row's detail has none.
        screen.press(KeyCode::Down);
        assert!(!screen.text().contains("Cannot quit"));
    }
}

#[test]
fn a_quit_that_did_not_end_the_process_shows_the_engine_outcome() {
    let cases = [
        (
            TerminateOutcome::StillRunning,
            "● Google Chrome Helper (Renderer) (PID 17703) is still running. X force quits it.",
        ),
        (
            TerminateOutcome::PermissionDenied,
            "● macOS denied the signal to Google Chrome Helper (Renderer) (PID 17703).",
        ),
        (
            TerminateOutcome::IdentityChanged,
            "● PID 17703 now belongs to another process. Nothing was sent.",
        ),
    ];
    for (outcome, line) in cases {
        let (mut screen, probe) = open_with(normal(), None, outcome, WIDE);
        screen
            .press(KeyCode::Char('x'))
            .press(KeyCode::Right)
            .press(KeyCode::Enter)
            .settle();
        let text = screen.text();
        assert!(text.contains(line), "{text}");
        assert_eq!(probe.reads.load(Ordering::SeqCst), 2);
    }
}
