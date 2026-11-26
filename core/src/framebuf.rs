//! 800×480 frame buffer for the Spectra 6 panel, 4 bits per pixel.
//!
//! Each nibble holds the panel's own colour code, so the buffer can be sent to
//! the ED2208 controller as-is (two pixels per byte, left pixel in the high nibble).

use embedded_graphics::pixelcolor::{PixelColor, Rgb888};
use embedded_graphics::prelude::*;

pub const WIDTH: u32 = 800;
pub const HEIGHT: u32 = 480;

/// The six inks of the Spectra 6 panel. Discriminants are the controller's colour codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Ink {
    Black = 0x0,
    White = 0x1,
    Yellow = 0x2,
    Red = 0x3,
    Blue = 0x5,
    Green = 0x6,
}

impl PixelColor for Ink {
    type Raw = ();
}

impl Ink {
    pub const ALL: [Ink; 6] = [Ink::Black, Ink::White, Ink::Yellow, Ink::Red, Ink::Blue, Ink::Green];

    /// Controller colour code (the nibble stored in the buffer).
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Ink> {
        Ink::ALL.into_iter().find(|i| i.code() == code)
    }

    /// How the ink actually looks on the panel (approximate measured values,
    /// not pure RGB). Used as the dithering target and for the PC preview.
    /// Tune these on the real device if photos look off.
    pub fn rgb(self) -> Rgb888 {
        match self {
            Ink::Black => Rgb888::new(25, 30, 33),
            Ink::White => Rgb888::new(232, 232, 232),
            Ink::Yellow => Rgb888::new(239, 222, 68),
            Ink::Red => Rgb888::new(178, 19, 24),
            Ink::Blue => Rgb888::new(33, 87, 186),
            Ink::Green => Rgb888::new(18, 95, 32),
        }
    }
}

#[derive(Clone)]
pub struct FrameBuffer {
    data: Vec<u8>,
}

impl FrameBuffer {
    pub fn new(fill: Ink) -> Self {
        let c = fill.code();
        Self { data: vec![(c << 4) | c; (WIDTH * HEIGHT / 2) as usize] }
    }

    /// Byte index and whether the pixel is the high (left) nibble.
    fn locate(x: u32, y: u32) -> (usize, bool) {
        assert!(x < WIDTH && y < HEIGHT, "pixel ({x},{y}) out of bounds");
        let i = (y * WIDTH + x) as usize;
        (i / 2, i.is_multiple_of(2))
    }

    /// Panics if out of bounds.
    pub fn get(&self, x: u32, y: u32) -> Ink {
        let (i, high) = Self::locate(x, y);
        let code = if high { self.data[i] >> 4 } else { self.data[i] & 0x0F };
        Ink::from_code(code).expect("buffer only ever holds valid ink codes")
    }

    /// Panics if out of bounds.
    pub fn set(&mut self, x: u32, y: u32, ink: Ink) {
        let (i, high) = Self::locate(x, y);
        let b = &mut self.data[i];
        *b = if high { (*b & 0x0F) | (ink.code() << 4) } else { (*b & 0xF0) | ink.code() };
    }

    /// Six vertical bars in `Ink::ALL` order (panel test pattern).
    pub fn colour_bars() -> FrameBuffer {
        let mut fb = FrameBuffer::new(Ink::White);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                fb.set(x, y, Ink::ALL[(x * 6 / WIDTH) as usize]);
            }
        }
        fb
    }

    /// Inverse of `as_bytes` (takes the buffer over, no copy); `None` for a
    /// wrong length or an invalid ink code.
    pub fn from_vec(bytes: Vec<u8>) -> Option<FrameBuffer> {
        let valid = |n: u8| Ink::from_code(n).is_some();
        (bytes.len() == (WIDTH * HEIGHT / 2) as usize && bytes.iter().all(|&b| valid(b >> 4) && valid(b & 0x0F)))
            .then_some(FrameBuffer { data: bytes })
    }

    /// Raw panel data, 4 bpp, 192 000 bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl OriginDimensions for FrameBuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for FrameBuffer {
    type Color = Ink;
    type Error = core::convert::Infallible;

    /// Pixels outside the screen are ignored.
    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, ink) in pixels {
            if let (Ok(x), Ok(y)) = (u32::try_from(p.x), u32::try_from(p.y)) {
                if x < WIDTH && y < HEIGHT {
                    self.set(x, y, ink);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_filled_with_one_ink() {
        let fb = FrameBuffer::new(Ink::White);
        assert_eq!(fb.as_bytes().len(), (WIDTH * HEIGHT / 2) as usize);
        assert!(fb.as_bytes().iter().all(|&b| b == 0x11));
        assert_eq!(fb.get(0, 0), Ink::White);
        assert_eq!(fb.get(WIDTH - 1, HEIGHT - 1), Ink::White);
    }

    #[test]
    fn set_changes_only_that_pixel() {
        let mut fb = FrameBuffer::new(Ink::White);
        fb.set(0, 0, Ink::Red);
        fb.set(3, 0, Ink::Blue);
        fb.set(WIDTH - 1, HEIGHT - 1, Ink::Green);
        assert_eq!(fb.get(0, 0), Ink::Red);
        assert_eq!(fb.get(1, 0), Ink::White);
        assert_eq!(fb.get(2, 0), Ink::White);
        assert_eq!(fb.get(3, 0), Ink::Blue);
        assert_eq!(fb.get(WIDTH - 1, HEIGHT - 1), Ink::Green);
        assert_eq!(fb.get(WIDTH - 2, HEIGHT - 1), Ink::White);
        // left pixel in the high nibble
        assert_eq!(fb.as_bytes()[0], (Ink::Red.code() << 4) | Ink::White.code());
    }

    #[test]
    fn ink_codes_match_the_panel_and_round_trip() {
        assert_eq!(Ink::Black.code(), 0);
        assert_eq!(Ink::Blue.code(), 5);
        for ink in Ink::ALL {
            assert_eq!(Ink::from_code(ink.code()), Some(ink));
        }
        assert_eq!(Ink::from_code(4), None);
    }

    #[test]
    fn drawing_outside_the_screen_is_ignored() {
        let mut fb = FrameBuffer::new(Ink::White);
        let pixels = [
            Pixel(Point::new(-1, 0), Ink::Black),
            Pixel(Point::new(0, -1), Ink::Black),
            Pixel(Point::new(WIDTH as i32, 0), Ink::Black),
            Pixel(Point::new(0, HEIGHT as i32), Ink::Black),
            Pixel(Point::new(5, 5), Ink::Black),
        ];
        fb.draw_iter(pixels).unwrap();
        assert_eq!(fb.get(5, 5), Ink::Black);
        let blacks = fb.as_bytes().iter().filter(|&&b| b != 0x11).count();
        assert_eq!(blacks, 1);
    }

    #[test]
    fn colour_bars_show_every_ink() {
        let fb = FrameBuffer::colour_bars();
        for (i, ink) in Ink::ALL.iter().enumerate() {
            for y in [0, HEIGHT - 1] {
                assert_eq!(fb.get(66 + 133 * i as u32, y), *ink, "bar {i}");
            }
        }
    }

    #[test]
    fn framebuffer_bytes_round_trip() {
        let fb = FrameBuffer::colour_bars();
        let back = FrameBuffer::from_vec(fb.as_bytes().to_vec()).expect("valid");
        assert_eq!(back.as_bytes(), fb.as_bytes());
        assert!(FrameBuffer::from_vec(fb.as_bytes()[1..].to_vec()).is_none(), "wrong length");
        let mut bad = fb.as_bytes().to_vec();
        bad[10] = 0x41; // nibble 4 is not an ink
        assert!(FrameBuffer::from_vec(bad).is_none());
    }

    #[test]
    fn panel_colours_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for ink in Ink::ALL {
            assert!(seen.insert(ink.rgb()), "{ink:?} duplicates another ink colour");
        }
    }
}
