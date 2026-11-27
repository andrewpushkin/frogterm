//! Rasterizes the text font (Rubik, SIL OFL, `fonts/`) into a 1-bit glyph
//! table at build time: the panel has no grey, and the ESP32 then only copies
//! bits (no font engine on the device).

use std::fmt::Write as _;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, VariableFont};

const FONT: &str = "fonts/Rubik-wght.ttf";
/// SemiBold: the lightest weight whose stems stay 2 px wide at this size.
const WEIGHT: f32 = 600.0;
/// Line height in pixels (ascent + descent).
const SIZE_PX: f32 = 15.0;

/// Latin, Latin-1, Cyrillic (Russian, Ukrainian), Hebrew and common punctuation.
fn chars() -> impl Iterator<Item = char> {
    let ranges = [(0x20, 0x7E), (0xA0, 0xFF), (0x400, 0x45F), (0x490, 0x491), (0x5D0, 0x5EA), (0x5F3, 0x5F4)];
    let extra = [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x201E, 0x2022, 0x2026, 0x20AA, 0x20AC, 0x2116];
    ranges.into_iter().flat_map(|(a, b)| a..=b).chain(extra).filter_map(char::from_u32)
}

fn main() {
    println!("cargo:rerun-if-changed={FONT}");
    println!("cargo:rerun-if-changed=build.rs");
    let mut font = FontVec::try_from_vec(std::fs::read(FONT).expect("read font")).expect("parse font");
    font.set_variation(b"wght", WEIGHT);
    let scale = PxScale::from(SIZE_PX);
    let sf = font.as_scaled(scale);
    let ascent = sf.ascent().round() as i32;

    let mut glyphs = String::new();
    let mut bits: Vec<u8> = Vec::new();
    for ch in chars() {
        let id = font.glyph_id(ch);
        if id.0 == 0 {
            continue; // not in the font
        }
        let advance = sf.h_advance(id).round() as i32;
        let (mut x, mut y, mut w, mut h) = (0, 0, 0, 0);
        let offset = bits.len();
        if let Some(o) = font.outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(0.0, ascent as f32))) {
            let b = o.px_bounds();
            (x, y, w, h) = (b.min.x as i32, b.min.y as i32, b.width() as usize, b.height() as usize);
            let mut on = vec![false; w * h];
            o.draw(|gx, gy, c| on[gy as usize * w + gx as usize] = c >= 0.5);
            // row-major, MSB first, the glyph starts on a byte boundary
            for chunk in on.chunks(8) {
                bits.push(chunk.iter().enumerate().fold(0u8, |acc, (i, &b)| acc | (u8::from(b) << (7 - i))));
            }
        }
        writeln!(glyphs, "    Glyph {{ ch: {:?}, advance: {advance}, x: {x}, y: {y}, w: {w}, h: {h}, bits: {offset} }},", ch).unwrap();
    }

    let out = format!(
        "/// Sorted by `ch`.\npub static GLYPHS: &[Glyph] = &[\n{glyphs}];\n\
         pub static BITS: &[u8] = &{bits:?};\n"
    );
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("font.rs");
    std::fs::write(path, out).expect("write font table");
}
