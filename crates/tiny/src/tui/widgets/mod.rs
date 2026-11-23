//! Drawing pieces shared by every page. Each returns [`StyledLine`](super::styled_line::StyledLine)s
//! or an [`Overlay`](overlay_box::Overlay); none draws to the terminal itself.

pub mod action_menu;
pub mod confirm_box;
pub mod list_cursor;
pub mod list_window;
pub mod meter;
pub mod overlay_box;
pub mod size_format;
pub mod spinner;
pub mod text_fit;
