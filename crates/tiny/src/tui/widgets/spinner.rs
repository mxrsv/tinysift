//! The one motion in the TUI: a braille spinner beside work in progress.
//! It turns only while a page reports [`is_busy`](crate::tui::pages::Page::is_busy).

/// Frames in turning order, starting on the glyph the mock draws.
const FRAMES: [char; 10] = ['⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '⠋', '⠙'];

/// The spinner glyph for a frame-loop tick.
pub fn spinner_glyph(tick: u64) -> char {
    FRAMES[(tick % FRAMES.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_is_the_mock_glyph_and_frames_cycle() {
        assert_eq!(spinner_glyph(0), '⠹');
        assert_eq!(spinner_glyph(10), '⠹');
        assert_ne!(spinner_glyph(1), spinner_glyph(0));
    }
}
