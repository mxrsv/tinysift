//! Type erasure between the frame and the pages: the frame holds five
//! `Box<dyn AnyPage>`, each page keeps its own message type.

use std::any::Any;

use crossterm::event::KeyEvent;

use super::contract::{KeyReply, Page, PageCtx, PageEnv, PageId, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;

/// [`Page`] without its message type, as the frame sees it.
pub trait AnyPage {
    fn id(&self) -> PageId;
    fn view(&self, ctx: &ViewCtx) -> PageView;
    fn on_show(&mut self, env: &mut PageEnv);
    fn on_reload(&mut self, env: &mut PageEnv);
    fn on_key(&mut self, key: KeyEvent, env: &mut PageEnv) -> KeyReply;
    fn on_message(&mut self, message: Box<dyn Any + Send>, env: &mut PageEnv);
    fn on_task_failed(&mut self, reason: &str, env: &mut PageEnv);
    fn is_busy(&self) -> bool;
    fn on_quit_request(&mut self, env: &mut PageEnv);
    fn holds_exit(&self) -> bool;
    fn exit_card(&self) -> Option<Vec<StyledLine>>;
}

/// Wraps a page for the registry.
pub fn register<P: Page + 'static>(page: P) -> Box<dyn AnyPage> {
    Box::new(Registered(page))
}

struct Registered<P>(P);

impl<P: Page> AnyPage for Registered<P> {
    fn id(&self) -> PageId {
        P::ID
    }

    fn view(&self, ctx: &ViewCtx) -> PageView {
        self.0.view(ctx)
    }

    fn on_show(&mut self, env: &mut PageEnv) {
        self.0.on_show(&mut PageCtx::new(env));
    }

    fn on_reload(&mut self, env: &mut PageEnv) {
        self.0.on_reload(&mut PageCtx::new(env));
    }

    fn on_key(&mut self, key: KeyEvent, env: &mut PageEnv) -> KeyReply {
        self.0.on_key(key, &mut PageCtx::new(env))
    }

    fn on_message(&mut self, message: Box<dyn Any + Send>, env: &mut PageEnv) {
        // Only this page's tasks send to this page, so the type always matches.
        if let Ok(message) = message.downcast::<P::Msg>() {
            self.0.on_message(*message, &mut PageCtx::new(env));
        }
    }

    fn on_task_failed(&mut self, reason: &str, env: &mut PageEnv) {
        self.0.on_task_failed(reason, &mut PageCtx::new(env));
    }

    fn is_busy(&self) -> bool {
        self.0.is_busy()
    }

    fn on_quit_request(&mut self, env: &mut PageEnv) {
        self.0.on_quit_request(&mut PageCtx::new(env));
    }

    fn holds_exit(&self) -> bool {
        self.0.holds_exit()
    }

    fn exit_card(&self) -> Option<Vec<StyledLine>> {
        self.0.exit_card()
    }
}
