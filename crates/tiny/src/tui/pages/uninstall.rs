//! Uninstall: the apps in /Applications (plan task 09).
//! Until it is built the tab draws the empty frame.

use super::{KeyHint, Page, PageId, PageView, ViewCtx};

pub struct UninstallPage;

pub fn page() -> UninstallPage {
    UninstallPage
}

impl Page for UninstallPage {
    const ID: PageId = PageId::Uninstall;
    type Msg = std::convert::Infallible;

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView {
            keys: KeyHint::globals(),
            ..PageView::default()
        }
    }
}
