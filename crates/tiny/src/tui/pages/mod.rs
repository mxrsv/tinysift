//! The five pages and the one place they are registered.
//!
//! Each page lives in its own module (`clean.rs` or `clean/mod.rs`, …) and
//! exposes `pub fn page() -> impl Page`. Building a page means editing only
//! its module: the registry below already calls every `page()`.

mod clean;
mod contract;
mod dispatch;
mod processes;
mod scan;
mod system;
mod uninstall;

#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "Detail and PageCtx are used by pages, plan tasks 05 to 09"
    )
)]
pub use contract::{
    Detail, FrameRequest, KeyHint, KeyReply, Page, PageCtx, PageEnv, PageId, PageView, ViewCtx,
};
#[cfg(test)]
pub use dispatch::register;
pub use dispatch::AnyPage;

/// Every page, in tab order (`PageId::ALL`).
pub fn all_pages() -> Vec<Box<dyn AnyPage>> {
    vec![
        dispatch::register(clean::page()),
        dispatch::register(uninstall::page()),
        dispatch::register(scan::page()),
        dispatch::register(processes::page()),
        dispatch::register(system::page()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_follows_tab_order() {
        let ids: Vec<_> = all_pages().iter().map(|page| page.id()).collect();
        assert_eq!(ids, PageId::ALL);
    }
}
