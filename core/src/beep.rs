//! Buzzer patterns (spec 7.2): success = one short high beep, error = three low
//! beeps, busy = three short high pips.

/// One tone; `hz == 0` is a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tone {
    pub hz: u32,
    pub ms: u32,
}

/// The final beep: the device is ready for the next press (good update).
pub const SUCCESS: &[Tone] = &[Tone { hz: 2000, ms: 150 }];
/// "Busy, press ignored": a press while the device works or the panel rests.
pub const BUSY: &[Tone] =
    &[Tone { hz: 2700, ms: 40 }, Tone { hz: 0, ms: 60 }, Tone { hz: 2700, ms: 40 }, Tone { hz: 0, ms: 60 }, Tone { hz: 2700, ms: 40 }];

/// Short chirp: "I saw the button press" (right after the button wakes the device).
pub const PRESS: &[Tone] = &[Tone { hz: 3000, ms: 50 }];
/// The final beep after a failed update (or a press that could not be done).
pub const ERROR: &[Tone] =
    &[Tone { hz: 1000, ms: 100 }, Tone { hz: 0, ms: 80 }, Tone { hz: 1000, ms: 100 }, Tone { hz: 0, ms: 80 }, Tone { hz: 1000, ms: 100 }];

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
    fn busy_is_an_audible_high_triple_pip_unlike_the_error() {
        // the piezo is loud only near its resonance (~2.5-4 kHz): a 1.2 kHz,
        // 30 ms tick was inaudible on the device
        let pips: Vec<&Tone> = BUSY.iter().filter(|t| t.hz > 0).collect();
        assert_eq!(pips.len(), 3);
        assert!(pips.iter().all(|t| (2500..=4000).contains(&t.hz) && t.ms >= 40));
        for other in [SUCCESS, PRESS, ERROR] {
            assert_ne!(BUSY, other);
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
