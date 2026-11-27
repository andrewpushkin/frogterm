//! Mixed Latin/Cyrillic/Hebrew text for event titles.
//!
//! Titles look like "Иврит / Язык (שפה) - учитель" or "תור טלפוני לד\"ר".
//! The panel has no text shaping, so we do a small subset of the Unicode bidi
//! algorithm: split into runs, put them in visual (left-to-right drawing) order,
//! and reverse Hebrew runs.
//!
//! The font is Rubik SemiBold (Latin, Cyrillic and Hebrew in one design),
//! rasterized to 1 bit at build time (`build.rs`).

use embedded_graphics::prelude::*;

use crate::framebuf::Ink;

/// One glyph of the build-time font table.
struct Glyph {
    ch: char,
    /// Pen advance in pixels.
    advance: i32,
    /// Bitmap offset from the pen position / line top.
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    /// Start of the bitmap in `BITS` (row-major, MSB first).
    bits: usize,
}

mod font {
    use super::Glyph;
    include!(concat!(env!("OUT_DIR"), "/font.rs"));
}

fn glyph(ch: char) -> Option<&'static Glyph> {
    font::GLYPHS.binary_search_by_key(&ch, |g| g.ch).ok().map(|i| &font::GLYPHS[i])
}

/// A piece of text in drawing order, and whether it needs the Hebrew font.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub hebrew: bool,
}

/// Runs in visual order (draw them left to right).
pub fn visual_runs(text: &str) -> Vec<Run> {
    // 1. segments of one class; a neutral between two segments of the same
    //    strong class joins them ("Иврит / Язык", "לד\"ר כהן")
    let mut segs: Vec<(Class, String)> = Vec::new();
    for ch in text.chars() {
        let class = classify(ch);
        match segs.last_mut() {
            Some((c, s)) if *c == class => s.push(ch),
            _ => {
                if class != Class::Neutral && segs.len() >= 2 {
                    let n = segs.len();
                    if segs[n - 1].0 == Class::Neutral && segs[n - 2].0 == class {
                        let (_, neutral) = segs.pop().expect("checked");
                        let last = &mut segs.last_mut().expect("checked").1;
                        last.push_str(&neutral);
                        last.push(ch);
                        continue;
                    }
                }
                segs.push((class, ch.to_string()));
            }
        }
    }
    // 2. visual order
    let rtl = segs.iter().find(|(c, _)| *c != Class::Neutral).is_some_and(|(c, _)| *c == Class::Hebrew);
    let reversed = |s: &str, mirror: bool| -> String {
        s.chars()
            .rev()
            .map(|c| match (mirror, c) {
                (true, '(') => ')',
                (true, ')') => '(',
                (true, '[') => ']',
                (true, ']') => '[',
                _ => c,
            })
            .collect()
    };
    let mut out = Vec::with_capacity(segs.len());
    let order: Box<dyn Iterator<Item = (Class, String)>> = if rtl { Box::new(segs.into_iter().rev()) } else { Box::new(segs.into_iter()) };
    for (class, s) in order {
        let (text, hebrew) = match class {
            Class::Hebrew => (reversed(&s, true), true),
            Class::Neutral if rtl => (reversed(&s, true), false),
            _ => (s, false),
        };
        out.push(Run { text, hebrew });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Hebrew,
    /// Left-to-right letters and digits (Latin, Cyrillic, numbers).
    Ltr,
    Neutral,
}

fn classify(ch: char) -> Class {
    match ch {
        '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}' => Class::Hebrew,
        c if c.is_alphanumeric() => Class::Ltr,
        _ => Class::Neutral,
    }
}

/// Draws `text` with its top-left at `pos`; returns the drawn width in
/// pixels. Characters missing from the font are skipped.
pub fn draw_mixed<D: DrawTarget<Color = Ink>>(target: &mut D, text: &str, pos: Point, ink: Ink) -> i32 {
    layout_glyphs(text, |g, x| {
        let origin = pos + Point::new(x + g.x, g.y);
        let pixels = (0..g.h).flat_map(move |row| (0..g.w).map(move |col| (row, col))).filter_map(move |(row, col)| {
            let bit = row * g.w + col;
            let on = font::BITS[g.bits + bit / 8] & (0x80 >> (bit % 8)) != 0;
            on.then(|| Pixel(origin + Point::new(col as i32, row as i32), ink))
        });
        let _ = target.draw_iter(pixels);
    })
}

/// Width `draw_mixed` would draw, without drawing (for right alignment).
pub fn mixed_width(text: &str) -> i32 {
    layout_glyphs(text, |_, _| {})
}

/// Walks the glyphs of `text` in visual order with their x offset, calling
/// `draw` for each; returns the total width.
fn layout_glyphs(text: &str, mut draw: impl FnMut(&Glyph, i32)) -> i32 {
    let mut x = 0;
    for run in visual_runs(text) {
        for g in run.text.chars().filter_map(glyph) {
            draw(g, x);
            x += g.advance;
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuf::FrameBuffer;

    fn joined(text: &str) -> String {
        visual_runs(text).into_iter().map(|r| r.text).collect()
    }

    #[test]
    fn plain_cyrillic_is_one_ltr_run() {
        assert_eq!(visual_runs("Акробатика"), vec![Run { text: "Акробатика".into(), hebrew: false }]);
    }

    #[test]
    fn hebrew_word_inside_russian_title_is_reversed_in_place() {
        assert_eq!(joined("Иврит / Язык (שפה) - учитель"), "Иврит / Язык (הפש) - учитель");
        let runs = visual_runs("Иврит (שפה)");
        assert!(runs.iter().any(|r| r.hebrew && r.text == "הפש"), "{runs:?}");
    }

    #[test]
    fn hebrew_title_is_drawn_right_to_left() {
        assert_eq!(joined("תור טלפוני"), "ינופלט רות");
        assert_eq!(joined("לד\"ר כהן"), "ןהכ ר\"דל");
    }

    #[test]
    fn numbers_in_hebrew_title_keep_their_order() {
        assert_eq!(joined("מפצח 2026"), "2026 חצפמ");
    }

    #[test]
    fn brackets_are_mirrored_in_hebrew_title() {
        assert_eq!(joined("שיעור (5)"), "(5) רועיש");
    }

    #[test]
    fn mixed_text_draws_glyphs_of_every_script() {
        for text in ["Акробатика", "שפה", "Hanukkah"] {
            let mut fb = FrameBuffer::new(Ink::White);
            let w = draw_mixed(&mut fb, text, Point::new(10, 10), Ink::Black);
            assert!(w > 10, "{text}: width {w}");
            let inked =
                (10..30).flat_map(|y| (10..10 + w as u32).map(move |x| (x, y))).filter(|&(x, y)| fb.get(x, y) == Ink::Black).count();
            assert!(inked > 10, "{text}: nothing drawn (missing glyphs?)");
        }
    }

    #[test]
    fn text_is_bold_strokes_two_pixels_wide() {
        // a vertical stroke ("l", "ו") is at least 2 px wide on every row it inks
        for text in ["l", "ו", "Н"] {
            let mut fb = FrameBuffer::new(Ink::White);
            draw_mixed(&mut fb, text, Point::new(10, 10), Ink::Black);
            let rows: Vec<Vec<u32>> = (10..30).map(|y| (8..30).filter(|&x| fb.get(x, y) == Ink::Black).collect()).collect();
            assert!(rows.iter().any(|r| !r.is_empty()), "{text}: nothing drawn");
            for r in rows.iter().filter(|r| !r.is_empty()) {
                assert!(r.len() >= 2, "{text}: a 1 px wide stroke {r:?}");
            }
        }
    }

    #[test]
    fn bold_letters_do_not_run_into_each_other() {
        // "ll": a blank column between the two strokes
        let mut fb = FrameBuffer::new(Ink::White);
        let w = draw_mixed(&mut fb, "ll", Point::new(10, 10), Ink::Black) as u32;
        let inked = |x: u32| (10..30).any(|y| fb.get(x, y) == Ink::Black);
        let cols: Vec<bool> = (10..10 + w).map(inked).collect();
        let first = cols.iter().position(|&c| c).unwrap();
        let gap = cols[first..].iter().position(|&c| !c).map(|g| first + g);
        assert!(gap.is_some_and(|g| cols[g..].iter().any(|&c| c)), "{cols:?}");
    }

    #[test]
    fn measured_width_matches_drawn_width() {
        for text in ["Тель-Авив 06:00", "תל אביב", "Tel Aviv 87%", ""] {
            let mut fb = FrameBuffer::new(Ink::White);
            assert_eq!(mixed_width(text), draw_mixed(&mut fb, text, Point::new(0, 0), Ink::Black), "{text}");
        }
    }
}
