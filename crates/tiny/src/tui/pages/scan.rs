//! Scan: large and old files, read on enter (plan task 06).
//! Until it is built the tab draws the empty frame.

use super::{KeyHint, Page, PageId, PageView, ViewCtx};

pub struct ScanPage;

pub fn page() -> ScanPage {
    ScanPage
}

impl Page for ScanPage {
    const ID: PageId = PageId::Scan;
    type Msg = std::convert::Infallible;

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView {
            keys: KeyHint::globals(),
            ..PageView::default()
        }
    }
}
