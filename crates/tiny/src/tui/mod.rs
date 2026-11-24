//! The full-screen interface bare `sift` opens in a terminal: five tabs
//! (Clean, Uninstall, Scan, Processes, System), the highlighted row's detail,
//! one key bar. It presents what `tiny-core` reports and adds no rule of its
//! own about what may be removed.
//!
//! Pages implement [`pages::Page`]; the frame, the shared widgets and the
//! test harness ([`page_harness`]) are what every page builds on.

mod app;
mod entry;
mod event_loop;
mod exit_print;
mod frame;
mod icons;
mod layout;
#[cfg(test)]
pub mod page_harness;
mod pages;
mod settings;
mod styled_line;
mod terminal;
mod theme;
mod widgets;
mod work;

pub use entry::{launch, Launch};
