//! States that exist only while a task is held: the run's ✓ list with the
//! spinner on the app in progress, and the one-at-a-time plan reads. The page
//! is driven directly so a test can handle queued answers while the fake
//! engine still holds a call at its gate.

use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::fake_engine::{Call, FakeEngine, FakeFrozen};
use super::tasks::Msg;
use super::UninstallPage;
use crate::tui::icons::IconMode;
use crate::tui::pages::{Page, PageCtx, PageEnv, PageId, PageView, ViewCtx};
use crate::tui::work::{LoopEvent, Spawner};

const WAIT: Duration = Duration::from_secs(10);

/// The page with its own message queue, at 80×24.
struct Direct {
    page: UninstallPage<FakeEngine>,
    env: PageEnv,
    rx: Receiver<LoopEvent>,
}

impl Direct {
    fn open(engine: Arc<FakeEngine>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut direct = Self {
            page: UninstallPage::new(engine),
            env: PageEnv {
                page: PageId::Uninstall,
                spawner: Spawner::new(tx),
                requests: Vec::new(),
                list_rows: 18,
            },
            rx,
        };
        direct.page.on_show(&mut PageCtx::new(&mut direct.env));
        direct
    }

    fn press(&mut self, code: KeyCode) -> &mut Self {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        self.page.on_key(key, &mut PageCtx::new(&mut self.env));
        self
    }

    fn view(&self) -> PageView {
        self.page.view(&ViewCtx {
            width: 80,
            height: 24,
            two_columns: false,
            list_width: 78,
            detail_width: 78,
            icons: IconMode::Unicode,
            tick: 0,
        })
    }

    fn rows(&self) -> Vec<String> {
        self.view()
            .list
            .iter()
            .map(|line| line.text().trim_end().to_string())
            .collect()
    }

    /// Handles answers until `done` holds.
    fn until(&mut self, done: impl Fn(&PageView) -> bool) -> &mut Self {
        let deadline = Instant::now() + WAIT;
        while !done(&self.view()) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                self.handle_next(left),
                "the page never reached the state:\n{:?}",
                self.rows()
            );
        }
        self
    }

    /// Handles the next answer a task sends; false if none came in `wait`.
    fn handle_next(&mut self, wait: Duration) -> bool {
        match self.rx.recv_timeout(wait) {
            Ok(LoopEvent::Page { message, .. }) => {
                let message = *message
                    .downcast::<Msg<FakeFrozen>>()
                    .unwrap_or_else(|_| panic!("a message of another page"));
                self.page
                    .on_message(message, &mut PageCtx::new(&mut self.env));
                true
            }
            Ok(_) => true,
            Err(_) => false,
        }
    }
}

fn plan_reads(engine: &FakeEngine) -> Vec<String> {
    engine
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Plan(app) => Some(app),
            _ => None,
        })
        .collect()
}

#[test]
fn a_run_shows_done_apps_the_one_in_progress_and_those_waiting() {
    let (engine, gate) = FakeEngine::normal().gated_removals();
    let engine = Arc::new(engine);
    let mut page = Direct::open(engine.clone());
    page.until(|view| view.detail.as_ref().is_some_and(|d| d.full.len() > 4));
    // AltTab, Pixelmator Pro, then Windows App (Keynote is refused).
    page.press(KeyCode::Char(' '))
        .press(KeyCode::Down)
        .press(KeyCode::Char(' '))
        .press(KeyCode::Down)
        .press(KeyCode::Down)
        .press(KeyCode::Char(' '))
        .until(|view| {
            view.foot
                .as_ref()
                .is_some_and(|f| f.text().contains("removable"))
        });
    page.press(KeyCode::Enter)
        .until(|view| view.overlay.is_some());
    gate.release(1);
    page.press(KeyCode::Enter);
    assert_eq!(gate.wait_entered(), "AltTab");
    assert_eq!(gate.wait_entered(), "Pixelmator Pro");
    page.until(|view| view.list[1].text().starts_with('⠹'));
    let rows = page.rows();
    assert!(
        rows[0].starts_with("✓ AltTab") && rows[0].ends_with("31 MB"),
        "{rows:?}"
    );
    assert!(rows[1].starts_with("⠹ Pixelmator Pro") && rows[1].ends_with("moving to Trash…"));
    assert_eq!(rows[2], "· Windows App");
    let view = page.view();
    assert_eq!(view.detail.unwrap().title, "Uninstalling");
    assert_eq!(view.keys[0].action, "stop after this app");
    assert!(page.page.holds_exit() && page.page.is_busy());

    gate.release(2);
    page.until(|view| view.detail.as_ref().is_some_and(|d| d.title == "Done"));
    assert!(!page.page.holds_exit());
    assert!(page.rows().iter().all(|row| row.starts_with('✓')));
}

#[test]
fn a_fast_cursor_reads_one_plan_at_a_time_and_skips_the_rows_it_passed() {
    let (engine, gate) = FakeEngine::normal().gated_plans();
    let engine = Arc::new(engine);
    let mut page = Direct::open(engine.clone());
    page.until(|view| view.cursor.is_some());
    assert_eq!(gate.wait_entered(), "AltTab");
    for _ in 0..5 {
        page.press(KeyCode::Down);
    }
    assert_eq!(plan_reads(&engine), ["AltTab"], "one read in flight");
    gate.release(1);
    assert!(page.handle_next(WAIT), "AltTab's plan arrives");
    assert_eq!(gate.wait_entered(), "Raycast");
    gate.release(1);
    page.until(|view| {
        view.detail
            .as_ref()
            .is_some_and(|d| d.full.iter().any(|l| l.text().starts_with("Removable")))
    });
    // Pixelmator Pro and Windows App were passed over; refused apps are never read.
    assert_eq!(plan_reads(&engine), ["AltTab", "Raycast"]);
}
