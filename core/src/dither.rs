//! Converts an RGB photo to the six panel inks with Floyd–Steinberg error diffusion.

use anyhow::{ensure, Result};
use embedded_graphics::pixelcolor::RgbColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use crate::framebuf::{FrameBuffer, Ink, HEIGHT, WIDTH};

/// Dither a full-screen RGB888 image (`WIDTH*HEIGHT*3` bytes, row-major) into `fb`.
///
/// Errors are diffused to the right (7/16) and to the next row (3/16, 5/16, 1/16),
/// measured against how the inks really look (`Ink::rgb`). Only two rows of
/// error are kept, so this is cheap enough for the ESP32.
pub fn dither_into(rgb: &[u8], fb: &mut FrameBuffer) -> Result<()> {
    dither_area(rgb, &Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT)), fb)
}

/// Dither an RGB888 image of exactly `area`'s size into that area of `fb`.
/// Each area is dithered on its own, so no error bleeds across area edges.
pub fn dither_area(rgb: &[u8], area: &Rectangle, fb: &mut FrameBuffer) -> Result<()> {
    let screen = Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT));
    ensure!(
        area.top_left.x >= 0 && area.top_left.y >= 0 && screen.intersection(area) == *area,
        "area {area:?} is not inside the screen"
    );
    let (w, h) = (area.size.width as usize, area.size.height as usize);
    let (x0, y0) = (area.top_left.x as u32, area.top_left.y as u32);
    ensure!(
        rgb.len() == w * h * 3,
        "expected a {w}x{h} RGB image ({} bytes), got {} bytes",
        w * h * 3,
        rgb.len()
    );
    // error accumulators in 1/16 units, one pixel of padding on each side
    let mut cur = vec![[0i32; 3]; w + 2];
    let mut next = vec![[0i32; 3]; w + 2];
    for y in 0..h {
        for x in 0..w {
            let src = &rgb[(y * w + x) * 3..][..3];
            let mut px = [0i32; 3];
            for c in 0..3 {
                px[c] = (i32::from(src[c]) + cur[x + 1][c] / 16).clamp(0, 255);
            }
            let ink = nearest_ink(px[0], px[1], px[2]);
            fb.set(x0 + x as u32, y0 + y as u32, ink);
            let got = ink.rgb();
            let got = [i32::from(got.r()), i32::from(got.g()), i32::from(got.b())];
            for c in 0..3 {
                let e = px[c] - got[c];
                cur[x + 2][c] += e * 7;
                next[x][c] += e * 3;
                next[x + 1][c] += e * 5;
                next[x + 2][c] += e;
            }
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|e| *e = [0; 3]);
    }
    Ok(())
}

/// "Frosted glass" under an overlay box: mixes the photo pixels of `area`
/// (`rgb` holds just that area) that fall inside `veil` with white.
/// `opacity` 0 = untouched, 100 = pure white. Call before dithering.
pub fn veil(rgb: &mut [u8], area: &Rectangle, veil: &Rectangle, opacity: u8) {
    let inter = area.intersection(veil);
    if inter.is_zero_sized() || opacity == 0 {
        return;
    }
    let o = u32::from(opacity.min(100));
    let w = area.size.width as usize;
    for y in inter.rows() {
        for x in inter.columns() {
            let i = ((y - area.top_left.y) as usize * w + (x - area.top_left.x) as usize) * 3;
            for v in &mut rgb[i..i + 3] {
                *v = ((u32::from(*v) * (100 - o) + 255 * o) / 100) as u8;
            }
        }
    }
}

/// The ink whose panel colour is closest to `(r, g, b)`.
pub fn nearest_ink(r: i32, g: i32, b: i32) -> Ink {
    Ink::ALL
        .into_iter()
        .min_by_key(|ink| {
            let c = ink.rgb();
            let (dr, dg, db) = (r - i32::from(c.r()), g - i32::from(c.g()), b - i32::from(c.b()));
            dr * dr + dg * dg + db * db
        })
        .expect("palette is not empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuf::{HEIGHT, WIDTH};
    use embedded_graphics::pixelcolor::RgbColor;

    fn image(f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                v.extend_from_slice(&f(x, y));
            }
        }
        v
    }

    fn luma(r: f64, g: f64, b: f64) -> f64 {
        0.299 * r + 0.587 * g + 0.114 * b
    }

    #[test]
    fn panel_colours_map_to_themselves() {
        // six vertical stripes, each exactly one ink's panel colour
        let img = image(|x, _| {
            let c = Ink::ALL[(x * 6 / WIDTH) as usize].rgb();
            [c.r(), c.g(), c.b()]
        });
        let mut fb = FrameBuffer::new(Ink::White);
        dither_into(&img, &mut fb).unwrap();
        for (i, ink) in Ink::ALL.iter().enumerate() {
            let x = (i as u32 * WIDTH / 6) + WIDTH / 12;
            for y in [0, HEIGHT / 2, HEIGHT - 1] {
                assert_eq!(fb.get(x, y), *ink, "stripe {i} at y={y}");
            }
        }
    }

    #[test]
    fn grey_keeps_its_average_brightness() {
        let img = image(|_, _| [128, 128, 128]);
        let mut fb = FrameBuffer::new(Ink::White);
        dither_into(&img, &mut fb).unwrap();
        let mut sum = 0.0;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let c = fb.get(x, y).rgb();
                sum += luma(c.r().into(), c.g().into(), c.b().into());
            }
        }
        let mean = sum / f64::from(WIDTH * HEIGHT);
        assert!((mean - 128.0).abs() < 128.0 * 0.05, "mean luma {mean}");
        // and it really is a mix, not one flat ink
        let block: std::collections::HashSet<Ink> =
            (0..8).flat_map(|y| (0..8).map(move |x| (x, y))).map(|(x, y)| fb.get(100 + x, 100 + y)).collect();
        assert!(block.len() >= 2, "grey became a single flat ink");
    }

    #[test]
    fn area_dither_touches_only_its_area() {
        use embedded_graphics::prelude::*;
        use embedded_graphics::primitives::Rectangle;
        let area = Rectangle::new(Point::new(402, 0), Size::new(398, HEIGHT));
        let black = Ink::Black.rgb();
        let img: Vec<u8> = (0..398 * HEIGHT).flat_map(|_| [black.r(), black.g(), black.b()]).collect();
        let mut fb = FrameBuffer::new(Ink::Red);
        dither_area(&img, &area, &mut fb).unwrap();
        for y in [0, HEIGHT / 2, HEIGHT - 1] {
            assert_eq!(fb.get(401, y), Ink::Red, "left of the area changed");
            assert_eq!(fb.get(402, y), Ink::Black);
            assert_eq!(fb.get(WIDTH - 1, y), Ink::Black);
            assert_eq!(fb.get(0, y), Ink::Red);
        }
    }

    #[test]
    fn area_must_match_image_and_fit_the_screen() {
        use embedded_graphics::prelude::*;
        use embedded_graphics::primitives::Rectangle;
        let mut fb = FrameBuffer::new(Ink::White);
        let img = vec![0u8; 10 * 10 * 3];
        assert!(dither_area(&img, &Rectangle::new(Point::new(0, 0), Size::new(10, 10)), &mut fb).is_ok());
        assert!(dither_area(&img, &Rectangle::new(Point::new(0, 0), Size::new(10, 11)), &mut fb).is_err());
        assert!(dither_area(&img, &Rectangle::new(Point::new(795, 0), Size::new(10, 10)), &mut fb).is_err());
        assert!(dither_area(&img, &Rectangle::new(Point::new(-1, 0), Size::new(10, 10)), &mut fb).is_err());
    }

    #[test]
    fn veil_lightens_only_inside_the_box() {
        let area = Rectangle::new(Point::new(400, 0), Size::new(10, 10));
        let boxed = Rectangle::new(Point::new(405, 5), Size::new(100, 100));
        let px = |rgb: &[u8], x: usize, y: usize| rgb[(y * 10 + x) * 3];
        for (opacity, want) in [(0u8, 100u8), (50, 177), (100, 255)] {
            let mut rgb = vec![100u8; 10 * 10 * 3];
            veil(&mut rgb, &area, &boxed, opacity);
            assert_eq!(px(&rgb, 4, 4), 100, "outside the box");
            assert_eq!(px(&rgb, 5, 5), want, "inside at {opacity} %");
            assert_eq!(px(&rgb, 9, 9), want);
        }
    }

    #[test]
    fn wrong_image_size_is_an_error() {
        let mut fb = FrameBuffer::new(Ink::White);
        assert!(dither_into(&[0u8; 30], &mut fb).is_err());
    }

    #[test]
    fn nearest_ink_picks_obvious_colours() {
        assert_eq!(nearest_ink(0, 0, 0), Ink::Black);
        assert_eq!(nearest_ink(255, 255, 255), Ink::White);
        assert_eq!(nearest_ink(250, 230, 0), Ink::Yellow);
        assert_eq!(nearest_ink(200, 20, 20), Ink::Red);
        assert_eq!(nearest_ink(20, 60, 200), Ink::Blue);
        assert_eq!(nearest_ink(40, 140, 60), Ink::Green);
    }
}
