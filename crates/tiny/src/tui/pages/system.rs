//! System: memory and disks (plan task 05).
//! Until it is built the tab draws the empty frame.

use super::{KeyHint, Page, PageId, PageView, ViewCtx};

pub struct SystemPage;

pub fn page() -> SystemPage {
    SystemPage
}

impl Page for SystemPage {
    const ID: PageId = PageId::System;
    type Msg = std::convert::Infallible;

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView {
            keys: KeyHint::globals(),
            ..PageView::default()
        }
    }
}
