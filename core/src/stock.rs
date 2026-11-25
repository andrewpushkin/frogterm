//! Which of the stored photo screens have been on the panel at least once.
//!
//! The green button and the daily update show a photo not seen yet; only when
//! every stored photo has been seen (also by browsing with the side buttons)
//! do they download a new batch.

/// Bit `i` set = stored photo `i` was shown. Batches have at most 20 photos.
pub type Seen = u32;

/// Marks stored photo `slot` as seen.
pub fn mark(seen: Seen, slot: u8) -> Seen {
    if slot < 32 {
        seen | 1 << slot
    } else {
        seen
    }
}

/// Stored photos of `count` not seen yet.
pub fn unseen(seen: Seen, count: u8) -> usize {
    (0..count).filter(|&i| !is_seen(seen, i)).count()
}

/// The first photo not seen yet after `current`, wrapping around; `None` when
/// all `count` were seen (time for a new batch).
pub fn next_unseen(seen: Seen, count: u8, current: u8) -> Option<u8> {
    (1..=count).map(|k| (current as usize + k as usize) % count as usize).map(|i| i as u8).find(|&i| !is_seen(seen, i))
}

/// The seen set kept by older firmware: photos `0..next` were shown in order.
pub fn from_counter(next: u8) -> Seen {
    (0..next.min(32)).fold(0, mark)
}

fn is_seen(seen: Seen, slot: u8) -> bool {
    slot < 32 && seen & 1 << slot != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_seen_shows_the_one_after_current() {
        assert_eq!(next_unseen(0, 10, 0), Some(1));
        assert_eq!(next_unseen(0, 10, 9), Some(0), "wraps around");
        assert_eq!(unseen(0, 10), 10);
    }

    #[test]
    fn browsed_photos_are_skipped() {
        // a new batch shows photo 0; the right button then showed 1 and 2
        let seen = [0, 1, 2].into_iter().fold(0, mark);
        assert_eq!(next_unseen(seen, 10, 2), Some(3));
        // the left button went back to 9 from 0: green continues after 9 with 3
        let seen = [0, 9].into_iter().fold(0, mark);
        assert_eq!(next_unseen(seen, 10, 9), Some(1));
        assert_eq!(unseen(seen, 10), 8);
    }

    #[test]
    fn all_seen_means_a_new_batch() {
        let all = (0..10).fold(0, mark);
        assert_eq!(next_unseen(all, 10, 4), None);
        assert_eq!(unseen(all, 10), 0);
        assert_eq!(next_unseen(0, 0, 0), None, "nothing stored");
    }

    #[test]
    fn bits_of_a_bigger_old_batch_do_not_count() {
        // seen 0..15 of an old batch of 15, now only 5 stored and all seen
        let seen = (0..15).fold(0, mark);
        assert_eq!(next_unseen(seen, 5, 0), None);
        assert_eq!(unseen(seen, 5), 0);
    }

    #[test]
    fn counter_of_older_firmware_converts() {
        assert_eq!(from_counter(0), 0);
        assert_eq!(next_unseen(from_counter(3), 10, 2), Some(3));
        assert_eq!(next_unseen(from_counter(10), 10, 9), None);
    }

    #[test]
    fn out_of_range_slots_are_ignored() {
        assert_eq!(mark(0, 40), 0);
    }
}
