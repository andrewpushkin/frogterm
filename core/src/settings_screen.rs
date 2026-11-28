//! The panel in settings mode: which Wi-Fi network to join and which address
//! to open, big enough to read from across the room.
//!
//! ```text
//! frogterm settings
//!
//! Wi-Fi network
//! frogterm-setup
//!
//! Open in a browser
//! http://192.168.4.1/
//!
//! Press the green button to finish.
//! ```

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::FontRenderer;

use crate::framebuf::{FrameBuffer, Ink, WIDTH};
use crate::text::draw_mixed;

static BIG: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB24_tf>();
static LABEL: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR18_tf>();

const LEFT: i32 = 60;

/// Draws the settings screen over the whole panel.
pub fn draw_settings_screen(fb: &mut FrameBuffer, network: &str, address: &str) {
    *fb = FrameBuffer::new(Ink::White);
    let text = |fb: &mut FrameBuffer, font: &FontRenderer, s: &str, y: i32, ink: Ink| {
        let _ =
            font.render_aligned(s, Point::new(LEFT, y), VerticalPosition::Top, HorizontalAlignment::Left, FontColor::Transparent(ink), fb);
    };
    text(fb, &BIG, "frogterm settings", 40, Ink::Black);
    let _ = Rectangle::new(Point::new(LEFT, 84), Size::new(WIDTH - 2 * LEFT as u32, 4))
        .into_styled(PrimitiveStyle::with_fill(Ink::Green))
        .draw(fb);
    text(fb, &LABEL, "Wi-Fi network", 130, Ink::Black);
    if network.chars().all(|c| (c as u32) < 0x100) {
        text(fb, &BIG, network, 168, Ink::Blue);
    } else {
        // Hebrew, Cyrillic…: the events font (smaller, but every letter)
        draw_mixed(fb, network, Point::new(LEFT, 172), Ink::Blue);
    }
    text(fb, &LABEL, "Open in a browser", 250, Ink::Black);
    text(fb, &BIG, address, 288, Ink::Blue);
    text(fb, &LABEL, "Press the green button to finish.", 400, Ink::Black);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuf::{Ink, HEIGHT, WIDTH};

    fn inked_rows(fb: &FrameBuffer, ink: Ink) -> Vec<u32> {
        (0..HEIGHT).filter(|&y| (0..WIDTH).any(|x| fb.get(x, y) == ink)).collect()
    }

    #[test]
    fn settings_screen_shows_network_and_address() {
        let mut fb = FrameBuffer::new(Ink::Red);
        draw_settings_screen(&mut fb, "frogterm-setup", "http://192.168.4.1/");
        assert!(inked_rows(&fb, Ink::Red).is_empty(), "the whole panel is redrawn, nothing red");
        assert!(inked_rows(&fb, Ink::Black).len() > 40, "title and labels in black");
        let blue = inked_rows(&fb, Ink::Blue);
        assert!(blue.len() > 30, "network and address in blue, big");
        let mut other = FrameBuffer::new(Ink::White);
        draw_settings_screen(&mut other, "frogterm-setup", "http://10.0.0.7:8080/");
        assert_ne!(fb.as_bytes(), other.as_bytes(), "the address is on the screen");
    }

    #[test]
    fn a_network_name_in_hebrew_or_cyrillic_is_drawn_too() {
        let mut fb = FrameBuffer::new(Ink::White);
        draw_settings_screen(&mut fb, "", "http://192.168.4.1/");
        let mut named = FrameBuffer::new(Ink::White);
        draw_settings_screen(&mut named, "בית Дом", "http://192.168.4.1/");
        assert_ne!(fb.as_bytes(), named.as_bytes());
    }
}
