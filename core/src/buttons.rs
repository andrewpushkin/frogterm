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
}
