//! Mixed Latin/Cyrillic/Hebrew text for event titles.
//!
//! Titles look like "Иврит / Язык (שפה) - учитель" or "תור טלפוני לד\"ר".
//! The panel has no text shaping, so we do a small subset of the Unicode bidi
//! algorithm: split into runs, put them in visual (left-to-right drawing) order,
//! reverse Hebrew runs, and draw each run with a font that has its glyphs.

use embedded_graphics::prelude::*;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, VerticalPosition};
use u8g2_fonts::FontRenderer;

use crate::framebuf::Ink;

static LATIN_CYRILLIC: FontRenderer = FontRenderer::new::<fonts::u8g2_font_cu12_t_cyrillic>();
static HEBREW: FontRenderer = FontRenderer::new::<fonts::u8g2_font_cu12_t_hebrew>();

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
    let order: Box<dyn Iterator<Item = &(Class, String)>> =
        if rtl { Box::new(segs.iter().rev()) } else { Box::new(segs.iter()) };
    for (class, s) in order {
        let (text, hebrew) = match class {
            Class::Hebrew => (reversed(s, true), true),
            Class::Neutral if rtl => (reversed(s, true), false),
            _ => (s.clone(), false),
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

/// Bold: every glyph is drawn twice, 1 px apart, and letters get 1 px more
/// room so they don't run together. (There is no bold font with both
/// Cyrillic and Hebrew at this size.)
const BOLD: i32 = 1;

/// Draws `text` in bold with its top-left at `pos`; returns the drawn width
/// in pixels. Characters missing from both fonts are skipped.
pub fn draw_mixed<D: DrawTarget<Color = Ink>>(target: &mut D, text: &str, pos: Point, ink: Ink) -> i32 {
    layout_glyphs(text, |font, s, x| {
        for dx in 0..=BOLD {
            let at = pos + Point::new(x + dx, 0);
            font.render(s, at, VerticalPosition::Top, FontColor::Transparent(ink), target).ok()?;
        }
        Some(())
    })
}

/// Width `draw_mixed` would draw, without drawing (for right alignment).
pub fn mixed_width(text: &str) -> i32 {
    layout_glyphs(text, |_, _, _| Some(()))
}

/// Walks the characters of `text` in visual order with their font and x
/// offset, calling `draw` for each; returns the total width. A character
/// `draw` fails on (missing glyph) takes no room.
fn layout_glyphs(text: &str, mut draw: impl FnMut(&FontRenderer, &str, i32) -> Option<()>) -> i32 {
    let mut x = 0;
    let mut buf = [0u8; 4];
    for run in visual_runs(text) {
        let font = if run.hebrew { &HEBREW } else { &LATIN_CYRILLIC };
        for ch in run.text.chars() {
            let s: &str = ch.encode_utf8(&mut buf);
            let Ok(d) = font.get_rendered_dimensions(s, Point::zero(), VerticalPosition::Top) else { continue };
            if draw(font, s, x).is_some() {
                x += d.advance.x + BOLD;
            }
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
            let inked = (10..30).flat_map(|y| (10..10 + w as u32).map(move |x| (x, y))).filter(|&(x, y)| fb.get(x, y) == Ink::Black).count();
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
