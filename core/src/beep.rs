//! Buzzer patterns (spec 7.2): success = one short high beep, error = three low beeps.

/// One tone; `hz == 0` is a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tone {
    pub hz: u32,
    pub ms: u32,
}

pub const SUCCESS: &[Tone] = &[Tone { hz: 2000, ms: 150 }];
/// Progress tick at each step of a button-started update (Wi-Fi up,
/// downloads done, screen drawing).
pub const TICK: &[Tone] = &[Tone { hz: 2700, ms: 40 }, Tone { hz: 0, ms: 60 }, Tone { hz: 2700, ms: 40 }];

/// Short chirp: "I saw the button press" (right after the button wakes the device).
pub const PRESS: &[Tone] = &[Tone { hz: 3000, ms: 50 }];
pub const ERROR: &[Tone] = &[
    Tone { hz: 1000, ms: 100 },
    Tone { hz: 0, ms: 80 },
    Tone { hz: 1000, ms: 100 },
    Tone { hz: 0, ms: 80 },
    Tone { hz: 1000, ms: 100 },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_one_short_high_beep() {
        assert_eq!(SUCCESS, &[Tone { hz: 2000, ms: 150 }]);
    }

    #[test]
    fn press_is_a_short_chirp_unlike_the_other_sounds() {
        assert_eq!(PRESS.len(), 1);
        assert!(PRESS[0].ms <= 60, "short");
        assert!(PRESS[0].hz > SUCCESS[0].hz, "higher than the 'done' beep");
        assert_ne!(PRESS, SUCCESS);
        assert_ne!(PRESS, ERROR);
    }

    #[test]
    fn tick_is_an_audible_double_pip() {
        // the piezo is loud only near its resonance (~2.5-4 kHz): a 1.2 kHz,
        // 30 ms tick was inaudible on the device
        let pips: Vec<&Tone> = TICK.iter().filter(|t| t.hz > 0).collect();
        assert_eq!(pips.len(), 2, "pip-pip, unlike the single press chirp");
        assert!(pips.iter().all(|t| (2500..=4000).contains(&t.hz) && t.ms >= 40));
        for other in [SUCCESS, PRESS, ERROR] {
            assert_ne!(TICK, other);
        }
    }

    #[test]
    fn error_is_three_low_beeps() {
        let beeps: Vec<&Tone> = ERROR.iter().filter(|t| t.hz > 0).collect();
        assert_eq!(beeps.len(), 3);
        assert!(beeps.iter().all(|t| t.hz == 1000 && t.ms == 100));
        assert!(ERROR.windows(2).all(|w| (w[0].hz == 0) != (w[1].hz == 0)), "beeps are separated by pauses");
    }
}
