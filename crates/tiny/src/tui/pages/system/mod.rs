//! System: memory and each disk once, and "This Mac". Read when the tab first
//! shows and on `r`; nothing is selectable.

mod engine;
mod view;

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};

use super::{KeyHint, KeyReply, Page, PageCtx, PageId, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::spinner::spinner_line;
use crate::tui::work::TaskId;
use engine::{RealSystem, SystemEngine, SystemReading};

pub struct SystemPage<E> {
    engine: Arc<E>,
    reading: Option<SystemReading>,
    status: Status,
    /// The read in flight.
    task: Option<TaskId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Idle,
    Reading,
    Failed(String),
}

pub fn page() -> SystemPage<RealSystem> {
    SystemPage::new(RealSystem)
}

impl<E: SystemEngine> SystemPage<E> {
    fn new(engine: E) -> Self {
        Self {
            engine: Arc::new(engine),
            reading: None,
            status: Status::Idle,
            task: None,
        }
    }

    fn start_read(&mut self, ctx: &mut PageCtx<'_, SystemReading>) {
        if self.status == Status::Reading {
            return;
        }
        let engine = Arc::clone(&self.engine);
        self.status = match ctx.spawn(move |worker| {
            worker.send(engine.read());
        }) {
            Ok(task) => {
                self.task = Some(task.id());
                Status::Reading
            }
            Err(error) => Status::Failed(error.to_string()),
        };
    }

    fn keys(&self) -> Vec<KeyHint> {
        let read = (self.status != Status::Reading || self.reading.is_some())
            .then(|| KeyHint::new("r", "read again"));
        read.into_iter().chain(KeyHint::globals()).collect()
    }
}

impl<E: SystemEngine> Page for SystemPage<E> {
    const ID: PageId = PageId::System;
    type Msg = SystemReading;

    fn view(&self, ctx: &ViewCtx) -> PageView {
        let keys = self.keys();
        if let Status::Failed(reason) = &self.status {
            return PageView {
                list: vec![
                    StyledLine::new()
                        .bad("● ")
                        .hi("The system could not be read"),
                    StyledLine::new().dim(format!("  {reason}")),
                ],
                keys,
                ..PageView::default()
            };
        }
        match &self.reading {
            Some(reading) => PageView {
                list: view::rows(reading, ctx.list_width),
                detail: Some(view::detail(reading, ctx.detail_width)),
                keys,
                ..PageView::default()
            },
            None => PageView {
                list: vec![spinner_line(ctx.tick, "reading…")],
                keys,
                ..PageView::default()
            },
        }
    }

    fn on_show(&mut self, ctx: &mut PageCtx<'_, SystemReading>) {
        if self.reading.is_none() && self.status == Status::Idle {
            self.start_read(ctx);
        }
    }

    fn on_reload(&mut self, ctx: &mut PageCtx<'_, SystemReading>) {
        self.start_read(ctx);
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, SystemReading>) -> KeyReply {
        match key.code {
            KeyCode::Char('r') => {
                self.start_read(ctx);
                KeyReply::Handled
            }
            _ => KeyReply::Ignored,
        }
    }

    fn on_message(&mut self, reading: SystemReading, _ctx: &mut PageCtx<'_, SystemReading>) {
        self.reading = Some(reading);
        self.status = Status::Idle;
        self.task = None;
    }

    fn on_task_failed(
        &mut self,
        task: TaskId,
        reason: &str,
        _ctx: &mut PageCtx<'_, SystemReading>,
    ) {
        if self.task.take_if(|id| *id == task).is_some() {
            self.status = Status::Failed(reason.to_string());
        }
    }

    fn is_busy(&self) -> bool {
        self.status == Status::Reading
    }
}

#[cfg(test)]
mod tests;
