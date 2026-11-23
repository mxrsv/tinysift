//! Clean: the categories to clean, Safe only until `v` (plan task 07).
//! Until it is built the tab draws the empty frame.

use super::{KeyHint, Page, PageId, PageView, ViewCtx};

pub struct CleanPage;

pub fn page() -> CleanPage {
    CleanPage
}

impl Page for CleanPage {
    const ID: PageId = PageId::Clean;
    type Msg = std::convert::Infallible;

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView {
            keys: KeyHint::globals(),
            ..PageView::default()
        }
    }
}
