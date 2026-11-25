//! Which photo(s) fill the screen.
//!
//! A landscape photo fills the whole 800×480 screen. A tall (portrait or square)
//! photo would lose most of itself to that crop, so it shares the screen with a
//! second random tall photo, side by side, separated by a thin white gap.

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use crate::album::Photo;
use crate::framebuf::{HEIGHT, WIDTH};

/// White gap between two side-by-side photos.
pub const GAP: u32 = 4;

/// One photo and the screen area it fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Index into the photo list.
    pub photo: usize,
    pub area: Rectangle,
}

/// How many of the newest photos count as "fresh" (one Google page).
pub const FRESH: usize = 300;

/// Slots for the screen, starting from the randomly chosen `first` photo
/// (any photo of the album). A tall first photo gets a tall partner from the
/// `fresh` newest photos (the start of the list, which is newest first), or
/// from any tall photo if there is none among them.
/// `rand(n)` must return a random number in `0..n`.
pub fn plan(photos: &[Photo], first: usize, fresh: usize, mut rand: impl FnMut(usize) -> usize) -> Vec<Slot> {
    let full = vec![Slot { photo: first, area: Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT)) }];
    if !photos[first].is_tall() {
        return full;
    }
    let tall_others = |range: std::ops::Range<usize>| -> Vec<usize> {
        range.filter(|&i| i != first && photos[i].is_tall()).collect()
    };
    let mut partners = tall_others(0..fresh.min(photos.len()));
    if partners.is_empty() {
        partners = tall_others(0..photos.len());
    }
    if partners.is_empty() {
        return full;
    }
    let second = partners[rand(partners.len())];
    let half = (WIDTH - GAP) / 2;
    vec![
        Slot { photo: first, area: Rectangle::new(Point::zero(), Size::new(half, HEIGHT)) },
        Slot { photo: second, area: Rectangle::new(Point::new((half + GAP) as i32, 0), Size::new(half, HEIGHT)) },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(w: u32, h: u32) -> Photo {
        Photo { url: format!("u{w}x{h}"), width: w, height: h, taken: None }
    }

    fn full() -> Rectangle {
        Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT))
    }

    #[test]
    fn landscape_photo_fills_the_screen() {
        let photos = [photo(4000, 3000), photo(3000, 4000)];
        assert_eq!(plan(&photos, 0, photos.len(), |_| 0), vec![Slot { photo: 0, area: full() }]);
    }

    #[test]
    fn tall_photo_gets_a_tall_partner_side_by_side() {
        // index: 0 tall, 1 landscape, 2 tall, 3 landscape, 4 tall
        let photos = [photo(3000, 4000), photo(4000, 3000), photo(1080, 1920), photo(1920, 835), photo(100, 100)];
        let slots = plan(&photos, 2, photos.len(), |_| 0);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].photo, 2, "the chosen photo goes left");
        assert_eq!(slots[1].photo, 0, "partner is the first other tall photo when rand gives 0");
        let half = (WIDTH - GAP) / 2;
        assert_eq!(slots[0].area, Rectangle::new(Point::zero(), Size::new(half, HEIGHT)));
        assert_eq!(slots[1].area, Rectangle::new(Point::new((half + GAP) as i32, 0), Size::new(half, HEIGHT)));
        assert_eq!(slots[1].area.bottom_right().unwrap(), Point::new(WIDTH as i32 - 1, HEIGHT as i32 - 1));
    }

    #[test]
    fn partner_is_drawn_only_from_other_tall_photos() {
        let photos = [photo(3000, 4000), photo(4000, 3000), photo(1080, 1920), photo(1920, 835), photo(100, 100)];
        let mut asked = None;
        let slots = plan(&photos, 2, photos.len(), |n| {
            asked = Some(n);
            n - 1
        });
        assert_eq!(asked, Some(2), "two other tall photos to choose from");
        assert_eq!(slots[1].photo, 4);
    }

    #[test]
    fn partner_of_a_tall_photo_is_fresh() {
        // newest first: 0..2 are "fresh"; 1 is the only fresh tall photo
        let photos = [photo(4000, 3000), photo(3000, 4000), photo(4000, 3000), photo(1080, 1920), photo(1080, 1920)];
        let slots = plan(&photos, 4, 3, |n| n - 1);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].photo, 4, "the fully random photo");
        assert_eq!(slots[1].photo, 1, "partner from the 3 freshest");
    }

    #[test]
    fn without_fresh_tall_photos_any_tall_partner_will_do() {
        let photos = [photo(4000, 3000), photo(4000, 3000), photo(1080, 1920), photo(1080, 1920)];
        let slots = plan(&photos, 3, 2, |_| 0);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[1].photo, 2);
    }

    #[test]
    fn lone_tall_photo_fills_the_screen() {
        let photos = [photo(4000, 3000), photo(3000, 4000)];
        assert_eq!(plan(&photos, 1, photos.len(), |_| 0), vec![Slot { photo: 1, area: full() }]);
    }
}
