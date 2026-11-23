//! Processes: your processes, read once (plan task 05).
//! Until it is built the tab draws the empty frame.

use super::{KeyHint, Page, PageId, PageView, ViewCtx};

pub struct ProcessesPage;

pub fn page() -> ProcessesPage {
    ProcessesPage
}

impl Page for ProcessesPage {
    const ID: PageId = PageId::Processes;
    type Msg = std::convert::Infallible;

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView {
            keys: KeyHint::globals(),
            ..PageView::default()
        }
    }
}
