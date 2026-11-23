//! Scan against the mock's `scan` screen and its states, fed the mock's data
//! through a fake engine that never walks the disk or opens Finder.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use tiny_core::scan::types::{FileEntry, ScanData};

use super::engine::ScanEngine;
use super::{ScanMsg, ScanPage};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::{PageHarness, STANDARD, WIDE};
use crate::tui::pages::PageId;

const HOME: &str = "/Users/someone";
const GB: f64 = 1e9;
const DAY: u64 = 86_400;

struct FakeScan {
    result: Result<ScanData, String>,
    /// While set, a scan waits until the test sends on the other end.
    gate: Option<Mutex<Receiver<()>>>,
    scans: Arc<AtomicUsize>,
    revealed: Arc<Mutex<Vec<PathBuf>>>,
}

impl ScanEngine for FakeScan {
    fn scan(&self, _progress: &(dyn Fn(String) + Send + Sync)) -> Result<ScanData, String> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let _ = gate.lock().unwrap().recv();
        }
        self.result.clone()
    }

    fn reveal(&self, path: &Path) -> Result<(), String> {
        self.revealed.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }

    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from(HOME))
    }
}

fn file(gb: f64, path: &str, days: u64) -> FileEntry {
    FileEntry {
        path: PathBuf::from(path.replacen('~', HOME, 1)),
        size: (gb * GB).round() as u64,
        age_secs: days * DAY + 3_600,
    }
}

/// `FILES` in `tools/mock.js`: the named files, then smaller generated ones
/// up to `large`, and `old` files that are not large.
fn files(named: &[(f64, &str, u64)], large: usize, old: usize) -> ScanData {
    let last = named.last().unwrap().0;
    let large: Vec<FileEntry> = (0..large)
        .map(|i| match named.get(i) {
            Some(&(gb, path, days)) => file(gb, path, days),
            None => file(
                (last * 0.9f64.powi((i - named.len() + 1) as i32)).max(0.1),
                &format!("~/Documents/archive/file-{i}.bin"),
                90 + i as u64,
            ),
        })
        .collect();
    let old = (0..old)
        .map(|i| FileEntry {
            path: PathBuf::from(format!("/o/{i}")),
            size: 1,
            age_secs: 200 * DAY,
        })
        .collect();
    ScanData {
        roots: ["Downloads", "Desktop", "Documents"]
            .iter()
            .map(|f| Path::new(HOME).join(f))
            .collect(),
        all_files: Vec::new(),
        large,
        old,
    }
}

fn normal() -> ScanData {
    files(
        &[
            (7.94, "~/Downloads/Windows11_Client_arm64_en-us.iso", 211),
            (3.1, "~/Downloads/Xcode_27_beta_4.xip", 96),
            (1.58, "~/Documents/Recordings/2026-07-13 11-57-38.mp4", 133),
            (1.32, "~/Documents/Recordings/day48-source-1080p.mp4", 12),
            (1.08, "~/Desktop/export-final.mov", 4),
            (0.64, "~/Downloads/dataset-images.zip", 301),
        ],
        85,
        15_882,
    )
}

fn worst() -> ScanData {
    files(
        &[
            (
                412.7,
                "~/Documents/Virtual Machines.localized/macOS 27 beta 4 clean install with a very long descriptive name.bundle/Disk Image Primary Auxiliary Storage.img",
                2911,
            ),
            (88.2, "~/Downloads/x.dmg", 0),
            (0.1, "~/Desktop/a.mov", 1),
        ],
        4_210,
        1_203_884,
    )
}

struct Probe {
    scans: Arc<AtomicUsize>,
    revealed: Arc<Mutex<Vec<PathBuf>>>,
    release: Option<Sender<()>>,
}

fn open_with(
    result: Result<ScanData, String>,
    gated: bool,
    size: (u16, u16),
    icons: IconMode,
) -> (PageHarness, Probe) {
    let (release, gate) = mpsc::channel();
    let probe = Probe {
        scans: Arc::default(),
        revealed: Arc::default(),
        release: gated.then_some(release),
    };
    let page = ScanPage::new(FakeScan {
        result,
        gate: gated.then(|| Mutex::new(gate)),
        scans: Arc::clone(&probe.scans),
        revealed: Arc::clone(&probe.revealed),
    });
    (PageHarness::new(page, size, icons), probe)
}

/// Opens the tab and presses enter, as the user starts a scan.
fn scanned(
    result: Result<ScanData, String>,
    size: (u16, u16),
    icons: IconMode,
) -> (PageHarness, Probe) {
    let (mut screen, probe) = open_with(result, false, size, icons);
    screen.press(KeyCode::Enter).settle();
    (screen, probe)
}

/// The mock's dump with `edits` applied, each a known difference named where it is used.
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

/// The harness reads `28.8 GB free`; the worst data `0.3 GB free`.
const WORST_FREE: (&str, &str) = (" 0.3 GB free", "28.8 GB free");

#[test]
fn opening_the_tab_scans_nothing_until_enter() {
    let (mut screen, probe) = open_with(Ok(normal()), false, STANDARD, IconMode::Unicode);
    let text = screen.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[2], " 100 MB and up · Downloads, Desktop, Documents");
    assert_eq!(
        lines[4],
        " enter scans Downloads, Desktop and Documents. It only reads."
    );
    assert_eq!(lines[23], " enter scan · tab page · q quit");
    screen.press(KeyCode::Tab).press(KeyCode::BackTab).settle();
    assert_eq!(probe.scans.load(Ordering::SeqCst), 0);
    screen.press(KeyCode::Enter).settle();
    assert_eq!(probe.scans.load(Ordering::SeqCst), 1);
}

#[test]
fn results_match_the_mock_at_both_sizes_and_in_nerd_mode() {
    let (mut screen, _) = scanned(Ok(normal()), STANDARD, IconMode::Unicode);
    assert_mock(&screen.text(), include_str!("fixtures/scan_std.txt"), &[]);
    let (mut screen, _) = scanned(Ok(normal()), WIDE, IconMode::Unicode);
    assert_mock(&screen.text(), include_str!("fixtures/scan_wide.txt"), &[]);
    let (mut screen, _) = scanned(Ok(normal()), WIDE, IconMode::Nerd);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/scan_wide_nerd.txt"),
        &[],
    );
}

#[test]
fn worst_data_matches_the_mock_at_both_sizes() {
    let (mut screen, _) = scanned(Ok(worst()), STANDARD, IconMode::Unicode);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/scan_worst_std.txt"),
        &[WORST_FREE],
    );
    let (mut screen, _) = scanned(Ok(worst()), WIDE, IconMode::Unicode);
    assert_mock(
        &screen.text(),
        include_str!("fixtures/scan_worst_wide.txt"),
        &[WORST_FREE],
    );
}

#[test]
fn scanning_shows_the_engine_message_and_esc_stops_without_a_result() {
    for (size, fixture) in [
        (STANDARD, include_str!("fixtures/scan_scanning_std.txt")),
        (WIDE, include_str!("fixtures/scan_scanning_wide.txt")),
    ] {
        let (mut screen, mut probe) = open_with(Ok(normal()), true, size, IconMode::Unicode);
        screen.press(KeyCode::Enter);
        // The engine reports what it does, not a running file count.
        screen.deliver(
            PageId::Scan,
            ScanMsg::Progress {
                run: 1,
                text: "Reading files".into(),
            },
        );
        assert_mock(
            &screen.text(),
            fixture,
            &[("reading… 48,210 files so far", "Reading files…")],
        );
        screen.press(KeyCode::Esc);
        probe.release.take().unwrap().send(()).unwrap();
        screen.settle();
        let text = screen.text();
        assert!(text.contains("enter scans Downloads"), "{text}");
        assert!(
            !text.contains("Windows11"),
            "the stopped scan's result is dropped"
        );
    }
}

#[test]
fn nothing_found_matches_the_mock() {
    let empty = || Ok(files(&[(0.0, "~/x", 0)], 0, 0));
    for (size, fixture) in [
        (STANDARD, include_str!("fixtures/scan_empty_std.txt")),
        (WIDE, include_str!("fixtures/scan_empty_wide.txt")),
    ] {
        let (mut screen, _) = scanned(empty(), size, IconMode::Unicode);
        assert_mock(&screen.text(), fixture, &[]);
    }
}

#[test]
fn an_error_shows_the_engine_reason_and_no_partial_list() {
    let error =
        "cannot read scan directory /Users/someone/Documents: Operation not permitted (os error 1)";
    let reason = "~/Documents could not be read\n   Nothing partial is shown. Allow access, then press r.\n\n";
    // The engine's own words replace the mock's; at 120 columns the list is 73 wide, so they wrap.
    let cases = [
        (
            STANDARD,
            include_str!("fixtures/scan_error_std.txt"),
            "cannot read scan directory ~/Documents: Operation not permitted (os error 1)\n   Nothing partial is shown. Allow access, then press r.\n\n",
        ),
        (
            WIDE,
            include_str!("fixtures/scan_error_wide.txt"),
            "cannot read scan directory ~/Documents: Operation not permitted (os\n   error 1)\n   Nothing partial is shown. Allow access, then press r.\n",
        ),
    ];
    for (size, fixture, engine_words) in cases {
        let (mut screen, _) = scanned(Err(error.into()), size, IconMode::Unicode);
        let text = screen.text();
        assert_mock(&text, fixture, &[(reason, engine_words)]);
        assert!(!text.contains('❯'), "no partial list");
    }
}

#[test]
fn v_switches_to_the_old_list_and_r_scans_again() {
    let (mut screen, probe) = scanned(Ok(normal()), STANDARD, IconMode::Unicode);
    screen.press(KeyCode::Char('v'));
    let text = screen.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[2], "  Large 85   Old 15,882");
    assert_eq!(
        lines[3],
        " untouched 90 days and up · Downloads, Desktop, Documents"
    );
    assert!(
        lines[5].starts_with(" ❯ under 1 KB   200d  /o/"),
        "{}",
        lines[5]
    );
    screen.press(KeyCode::Char('r')).settle();
    assert_eq!(probe.scans.load(Ordering::SeqCst), 2);
}

#[test]
fn o_shows_the_highlighted_file_in_finder_through_the_engine() {
    let (mut screen, probe) = scanned(Ok(normal()), WIDE, IconMode::Unicode);
    screen
        .press(KeyCode::Down)
        .press(KeyCode::Char('o'))
        .settle();
    assert_eq!(
        *probe.revealed.lock().unwrap(),
        [PathBuf::from(
            "/Users/someone/Downloads/Xcode_27_beta_4.xip"
        )]
    );
}
