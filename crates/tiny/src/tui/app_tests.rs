//! The global keys, quitting, routing and requests, through the real frame.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::tui::page_harness::{PageHarness, STANDARD};
use crate::tui::pages::{register, Page, PageCtx, PageView};

type Log = Rc<RefCell<Vec<String>>>;

/// A page that records what the frame asks of it.
struct Probe<const N: usize> {
    log: Log,
    /// Takes every key, as a page with an open box does.
    swallow: bool,
    holds: Rc<Cell<bool>>,
    card: bool,
}

impl<const N: usize> Probe<N> {
    fn note(&self, what: impl std::fmt::Display) {
        self.log
            .borrow_mut()
            .push(format!("{}:{what}", Self::ID.title()));
    }
}

impl<const N: usize> Page for Probe<N> {
    const ID: PageId = PageId::ALL[N];
    type Msg = String;

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView {
            keys: [KeyHint::new("r", "read again")]
                .into_iter()
                .chain(KeyHint::globals())
                .collect(),
            ..PageView::default()
        }
    }

    fn on_show(&mut self, _ctx: &mut PageCtx<'_, String>) {
        self.note("show");
    }

    fn on_reload(&mut self, _ctx: &mut PageCtx<'_, String>) {
        self.note("reload");
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, String>) -> KeyReply {
        match key.code {
            KeyCode::Char('r') => ctx.reload(PageId::Processes),
            KeyCode::Char('s') => ctx.show(PageId::System),
            KeyCode::Char('f') => ctx.refresh_free_space(),
            KeyCode::Char('i') => self.note(format!("icons {}", ctx.icons().name())),
            KeyCode::Char('w') => {
                ctx.spawn(|worker| {
                    worker.send("from a task".to_string());
                })
                .unwrap();
            }
            _ if self.swallow => {}
            _ => return KeyReply::Ignored,
        }
        KeyReply::Handled
    }

    fn on_message(&mut self, message: String, _ctx: &mut PageCtx<'_, String>) {
        self.note(message);
    }

    fn on_task_failed(&mut self, reason: &str, _ctx: &mut PageCtx<'_, String>) {
        self.note(format!("failed {reason}"));
    }

    fn on_quit_request(&mut self, _ctx: &mut PageCtx<'_, String>) {
        self.note("quit");
    }

    fn holds_exit(&self) -> bool {
        self.holds.get()
    }

    fn exit_card(&self) -> Option<Vec<StyledLine>> {
        self.card.then(|| {
            vec![StyledLine::new()
                .ok("✓ ")
                .hi(format!("{} card", Self::ID.title()))]
        })
    }
}

struct Session {
    screen: PageHarness,
    log: Log,
    holds: Rc<Cell<bool>>,
}

fn session(swallow_on_clean: bool) -> Session {
    let log = Log::default();
    let holds = Rc::new(Cell::new(false));
    let probe = |swallow, card| (log.clone(), swallow, holds.clone(), card);
    let (l0, s0, h0, c0) = probe(swallow_on_clean, true);
    let pages = vec![
        register(Probe::<0> {
            log: l0,
            swallow: s0,
            holds: h0,
            card: c0,
        }),
        register(Probe::<1> {
            log: log.clone(),
            swallow: false,
            holds: Rc::default(),
            card: false,
        }),
        register(Probe::<2> {
            log: log.clone(),
            swallow: false,
            holds: Rc::default(),
            card: false,
        }),
        register(Probe::<3> {
            log: log.clone(),
            swallow: false,
            holds: Rc::default(),
            card: false,
        }),
        register(Probe::<4> {
            log: log.clone(),
            swallow: false,
            holds: Rc::default(),
            card: true,
        }),
    ];
    let screen = PageHarness::with_pages(
        pages,
        PageId::Clean,
        STANDARD,
        crate::tui::icons::IconMode::Unicode,
    );
    Session { screen, log, holds }
}

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}

#[test]
fn opens_on_clean_and_tab_keys_cycle_the_five_pages() {
    let mut s = session(false);
    assert_eq!(s.screen.active(), PageId::Clean);
    let mut seen = vec![];
    for _ in 0..5 {
        s.screen.press(KeyCode::Tab);
        seen.push(s.screen.active());
    }
    assert_eq!(
        seen,
        [
            PageId::Uninstall,
            PageId::Scan,
            PageId::Processes,
            PageId::System,
            PageId::Clean
        ]
    );
    s.screen.press(KeyCode::BackTab);
    assert_eq!(s.screen.active(), PageId::System);
    s.screen.press(KeyCode::Char('3'));
    assert_eq!(s.screen.active(), PageId::Scan);
    s.screen.press(KeyCode::Char('9'));
    assert_eq!(s.screen.active(), PageId::Scan);
    assert_eq!(s.log.borrow()[0], "Clean:show");
    assert!(s.log.borrow().contains(&"Uninstall:show".to_string()));
}

#[test]
fn q_ctrl_c_and_signals_quit() {
    let mut s = session(false);
    s.screen.press(KeyCode::Char('q'));
    assert!(s.screen.exited());
    let mut s = session(false);
    s.screen.press_key(ctrl_c());
    assert!(s.screen.exited());
    let mut s = session(false);
    s.screen.signal();
    assert!(s.screen.exited());
}

#[test]
fn a_page_with_an_open_box_keeps_q_and_tab_but_not_ctrl_c() {
    let mut s = session(true);
    s.screen.press(KeyCode::Char('q')).press(KeyCode::Tab);
    assert!(!s.screen.exited());
    assert_eq!(s.screen.active(), PageId::Clean);
    s.screen.press_key(ctrl_c());
    assert!(s.screen.exited());
}

#[test]
fn quitting_waits_for_a_page_that_holds_the_exit() {
    let mut s = session(false);
    s.holds.set(true);
    s.screen.press(KeyCode::Char('q'));
    assert!(!s.screen.exited(), "a write in progress finishes first");
    let quits = s
        .log
        .borrow()
        .iter()
        .filter(|l| l.ends_with(":quit"))
        .count();
    assert_eq!(quits, 5, "every page is told once");
    s.screen.press(KeyCode::Tab);
    assert_eq!(
        s.screen.active(),
        PageId::Clean,
        "keys are ignored while quitting"
    );
    s.holds.set(false);
    s.screen
        .deliver(PageId::Clean, "last category done".to_string());
    assert!(s.screen.exited());
}

#[test]
fn question_mark_lists_the_keys_and_esc_closes_it() {
    let mut s = session(false);
    s.screen.press(KeyCode::Char('?'));
    let text = s.screen.text();
    assert!(text.contains("╭─ Keys "), "{text}");
    assert!(text.contains("│ r                 read again"), "{text}");
    assert!(text.contains("next or previous page"));
    assert!(text.ends_with(" esc close"));
    s.screen.press(KeyCode::Tab);
    assert_eq!(
        s.screen.active(),
        PageId::Clean,
        "tab does not leave the list of keys"
    );
    s.screen.press(KeyCode::Esc);
    assert!(!s.screen.text().contains("╭─ Keys "));
}

#[test]
fn messages_reach_their_page_while_another_tab_is_shown() {
    let mut s = session(false);
    s.screen
        .press(KeyCode::Tab)
        .deliver(PageId::System, "sampled".to_string());
    assert!(s.log.borrow().contains(&"System:sampled".to_string()));
}

#[test]
fn spawned_work_reports_back_through_on_message() {
    let mut s = session(false);
    s.screen.press(KeyCode::Char('w')).settle();
    assert!(s.log.borrow().contains(&"Clean:from a task".to_string()));
}

#[test]
fn reload_shows_the_page_and_asks_it_to_read_again() {
    let mut s = session(false);
    s.screen.press(KeyCode::Char('r'));
    assert_eq!(s.screen.active(), PageId::Processes);
    let log = s.log.borrow();
    let tail: Vec<_> = log.iter().rev().take(2).rev().cloned().collect();
    assert_eq!(tail, ["Processes:show", "Processes:reload"]);
}

#[test]
fn a_failed_task_reaches_its_page_and_the_notices() {
    let mut s = session(false);
    s.screen.fail(PageId::Scan, "walker broke");
    assert!(s
        .log
        .borrow()
        .contains(&"Scan:failed walker broke".to_string()));
    assert_eq!(s.screen.finish().notices, ["Scan: walker broke"]);
}

#[test]
fn show_switches_without_a_reload_and_the_context_answers() {
    let mut s = session(false);
    s.screen
        .press(KeyCode::Char('i'))
        .press(KeyCode::Char('f'))
        .settle();
    s.screen.press(KeyCode::Char('s'));
    assert_eq!(s.screen.active(), PageId::System);
    let log = s.log.borrow();
    assert!(log.contains(&"Clean:icons unicode".to_string()));
    assert_eq!(log.last().unwrap(), "System:show");
    assert!(!log.iter().any(|l| l.ends_with("reload")));
}

#[test]
fn exit_cards_are_collected_in_tab_order() {
    let s = session(false);
    let end = s.screen.finish();
    let text: Vec<String> = end.exit_card.iter().map(StyledLine::text).collect();
    assert_eq!(text, ["✓ Clean card", "✓ System card"]);
}
