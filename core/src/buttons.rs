//! The three top buttons (active low, all can wake the chip from deep sleep).

/// Green button: a stored photo not seen yet, or a new batch once all were
/// seen (and Wi-Fi when needed).
pub const GREEN_GPIO: u32 = 3;
/// Side buttons: browse the stored photos without Wi-Fi. Checked on the
/// device: the left button is GPIO4, the right one GPIO5.
pub const PREVIOUS_GPIO: u32 = 4;
pub const NEXT_GPIO: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Update,
    Next,
    Previous,
}

impl Press {
    /// Stored form (0 = no press).
    pub fn code(self) -> u8 {
        match self {
            Press::Update => 1,
            Press::Next => 2,
            Press::Previous => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<Press> {
        [Press::Update, Press::Next, Press::Previous].into_iter().find(|p| p.code() == code)
    }
}

/// Every button with its pin, green first.
pub const ALL: [(Press, u32); 3] = [(Press::Update, GREEN_GPIO), (Press::Next, NEXT_GPIO), (Press::Previous, PREVIOUS_GPIO)];

/// New presses while the device is awake, from the buttons' levels (in `ALL`
/// order) polled again and again.
pub struct NewPresses {
    down: [bool; 3],
}

impl NewPresses {
    /// Buttons already down now (e.g. the one that woke the device) are not new presses.
    pub fn new(down: [bool; 3]) -> NewPresses {
        NewPresses { down }
    }

    /// The button that went down since the last poll, if any.
    pub fn poll(&mut self, down: [bool; 3]) -> Option<Press> {
        let new = (0..3).find(|&i| down[i] && !self.down[i]).map(|i| ALL[i].0);
        self.down = down;
        new
    }
}

/// Green or right held this long: settings mode, at once (still held).
pub const HOLD_MS: u64 = 10_000;
/// Left still down after this long (stuck, or something lying on it) counts
/// as let go.
pub const STUCK_MS: u64 = 60_000;

/// What a button did. Every button acts on release; the one thing that
/// happens while a button is down is a `Hold` (green or right, 10 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Pressed and let go (before the hold time).
    Press(Press),
    /// Held `HOLD_MS`: green or right only.
    Hold(Press),
}

/// Presses and holds from the buttons' levels (in `ALL` order), polled
/// again and again with a millisecond clock.
pub struct Holds {
    /// Since when each button is down.
    since: [Option<u64>; 3],
    /// Its gesture already came (a hold): its release does nothing.
    done: [bool; 3],
}

impl Holds {
    /// Buttons down now count as pressed now (the press that woke the chip).
    pub fn new(down: [bool; 3], now_ms: u64) -> Holds {
        Holds { since: down.map(|d| d.then_some(now_ms)), done: [false; 3] }
    }

    /// The gesture since the last poll, if any.
    pub fn poll(&mut self, down: [bool; 3], now_ms: u64) -> Option<Gesture> {
        // one gesture per poll; a second button's change waits for the next one
        for (i, &is_down) in down.iter().enumerate() {
            let p = ALL[i].0;
            match (is_down, self.since[i]) {
                (true, None) => self.since[i] = Some(now_ms),
                (true, Some(t)) if !self.done[i] => {
                    let held = now_ms.saturating_sub(t);
                    let gesture = match p {
                        Press::Update | Press::Next if held >= HOLD_MS => Gesture::Hold(p),
                        Press::Previous if held >= STUCK_MS => Gesture::Press(p),
                        _ => continue,
                    };
                    self.done[i] = true;
                    return Some(gesture);
                }
                (true, Some(_)) => {}
                (false, Some(_)) => {
                    self.since[i] = None;
                    if !std::mem::take(&mut self.done[i]) {
                        return Some(Gesture::Press(p));
                    }
                }
                (false, None) => {}
            }
        }
        None
    }
}

/// Which button woke the chip, from the ext1 wake-up GPIO mask.
pub fn press_from_pins(mask: u64) -> Option<Press> {
    let pressed = |gpio: u32| mask & (1 << gpio) != 0;
    if pressed(GREEN_GPIO) {
        Some(Press::Update)
    } else if pressed(NEXT_GPIO) {
        Some(Press::Next)
    } else if pressed(PREVIOUS_GPIO) {
        Some(Press::Previous)
    } else {
        None
    }
}

/// Slot to show when browsing `count` stored photos from `current`, wrapping around.
pub fn browse(current: u8, count: u8, press: Press) -> Option<u8> {
    if count == 0 {
        return None;
    }
    let current = if current < count { current } else { count - 1 };
    match press {
        Press::Next => Some((current + 1) % count),
        Press::Previous => Some((current + count - 1) % count),
        Press::Update => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_pin_tells_the_button() {
        assert_eq!(press_from_pins(1 << GREEN_GPIO), Some(Press::Update));
        assert_eq!(press_from_pins(1 << NEXT_GPIO), Some(Press::Next));
        assert_eq!(press_from_pins(1 << PREVIOUS_GPIO), Some(Press::Previous));
        assert_eq!(press_from_pins(0), None);
        // two at once: the green button wins
        assert_eq!(press_from_pins(1 << GREEN_GPIO | 1 << NEXT_GPIO), Some(Press::Update));
    }

    #[test]
    fn only_a_button_going_down_is_a_new_press() {
        let mut p = NewPresses::new([true, false, false]);
        assert_eq!(p.poll([true, false, false]), None, "the press that woke the device is not new");
        assert_eq!(p.poll([false, false, false]), None, "letting go is no press");
        assert_eq!(p.poll([false, true, false]), Some(Press::Next));
        assert_eq!(p.poll([false, true, false]), None, "held, not pressed again");
        assert_eq!(p.poll([true, true, true]), Some(Press::Update));
        assert_eq!(p.poll([false, false, false]), None);
        assert_eq!(p.poll([false, false, true]), Some(Press::Previous));
    }

    #[test]
    fn press_codes_round_trip() {
        for p in [Press::Update, Press::Next, Press::Previous] {
            assert_ne!(p.code(), 0);
            assert_eq!(Press::from_code(p.code()), Some(p));
        }
        assert_eq!(Press::from_code(0), None);
    }

    #[test]
    fn browsing_wraps_around_the_stored_photos() {
        assert_eq!(browse(3, 10, Press::Next), Some(4));
        assert_eq!(browse(9, 10, Press::Next), Some(0));
        assert_eq!(browse(3, 10, Press::Previous), Some(2));
        assert_eq!(browse(0, 10, Press::Previous), Some(9));
        assert_eq!(browse(0, 0, Press::Next), None, "nothing stored");
        assert_eq!(browse(15, 10, Press::Next), Some(0), "stale index after a smaller batch");
        assert_eq!(browse(4, 10, Press::Update), None, "not a browse button");
    }

    /// Levels in `ALL` order: green, next (right), previous (left).
    const NONE: [bool; 3] = [false, false, false];
    const GREEN: [bool; 3] = [true, false, false];
    const RIGHT: [bool; 3] = [false, true, false];
    const LEFT: [bool; 3] = [false, false, true];

    #[test]
    fn a_press_counts_on_release() {
        let mut h = Holds::new(NONE, 0);
        assert_eq!(h.poll(RIGHT, 100), None, "nothing while down");
        assert_eq!(h.poll(RIGHT, 3000), None);
        assert_eq!(h.poll(NONE, 3020), Some(Gesture::Press(Press::Next)));
        assert_eq!(h.poll(NONE, 3040), None, "once");
        assert_eq!(h.poll(GREEN, 4000), None);
        assert_eq!(h.poll(NONE, 4000 + HOLD_MS - 1), Some(Gesture::Press(Press::Update)), "just short of the hold");
    }

    #[test]
    fn green_or_right_held_10_s_is_a_hold_at_once() {
        for (levels, p) in [(GREEN, Press::Update), (RIGHT, Press::Next)] {
            let mut h = Holds::new(NONE, 0);
            assert_eq!(h.poll(levels, 1000), None);
            assert_eq!(h.poll(levels, 1000 + HOLD_MS - 1), None);
            assert_eq!(h.poll(levels, 1000 + HOLD_MS), Some(Gesture::Hold(p)), "still down");
        }
        assert_eq!(HOLD_MS, 10_000);
    }

    #[test]
    fn the_release_after_a_hold_does_nothing() {
        let mut h = Holds::new(NONE, 0);
        h.poll(GREEN, 0);
        assert_eq!(h.poll(GREEN, HOLD_MS), Some(Gesture::Hold(Press::Update)));
        assert_eq!(h.poll(GREEN, HOLD_MS + 5000), None, "not again");
        assert_eq!(h.poll(NONE, HOLD_MS + 6000), None, "the release does nothing");
        assert_eq!(h.poll(GREEN, HOLD_MS + 7000), None);
        assert_eq!(h.poll(NONE, HOLD_MS + 7100), Some(Gesture::Press(Press::Update)), "the next press is a press again");
    }

    #[test]
    fn left_held_long_is_a_press_on_release() {
        let mut h = Holds::new(NONE, 0);
        h.poll(LEFT, 0);
        assert_eq!(h.poll(LEFT, 20_000), None, "no hold for left");
        assert_eq!(h.poll(NONE, 20_100), Some(Gesture::Press(Press::Previous)));
    }

    #[test]
    fn left_stuck_60_s_counts_as_released() {
        let mut h = Holds::new(NONE, 0);
        h.poll(LEFT, 0);
        assert_eq!(h.poll(LEFT, STUCK_MS - 1), None);
        assert_eq!(h.poll(LEFT, STUCK_MS), Some(Gesture::Press(Press::Previous)));
        assert_eq!(h.poll(NONE, STUCK_MS + 500), None, "the real release does nothing");
        assert_eq!(STUCK_MS, 60_000);
    }

    #[test]
    fn the_waking_button_is_pressed_from_the_start() {
        let mut h = Holds::new(GREEN, 500);
        assert_eq!(h.poll(GREEN, 600), None);
        assert_eq!(h.poll(NONE, 700), Some(Gesture::Press(Press::Update)));
        let mut h = Holds::new(RIGHT, 500);
        assert_eq!(h.poll(RIGHT, 500 + HOLD_MS), Some(Gesture::Hold(Press::Next)));
    }

    #[test]
    fn two_buttons_let_go_together_both_count() {
        let mut h = Holds::new(NONE, 0);
        h.poll([true, true, false], 0);
        let a = h.poll(NONE, 100);
        let b = h.poll(NONE, 120);
        assert_eq!((a, b), (Some(Gesture::Press(Press::Update)), Some(Gesture::Press(Press::Next))));
    }
}
