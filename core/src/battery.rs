//! Li-ion battery level from the measured voltage.

/// Approximate charge in percent for a cell voltage in millivolts.
pub fn percent(mv: u32) -> u8 {
    // (mV, %) - Seeed's calibration for the reTerminal E-series (ESPHome
    // cookbook), measured the same way (ADC1 GPIO1 x2), except the top point:
    // Seeed uses 4.15 V = 100 %, but a fully charged battery reads 4.08-4.09 V
    // on this board, so 4.08 V = 100 %.
    const CURVE: [(u32, u32); 12] = [
        (3270, 0),
        (3300, 5),
        (3410, 10),
        (3490, 20),
        (3580, 30),
        (3680, 40),
        (3750, 50),
        (3800, 60),
        (3850, 70),
        (3910, 80),
        (3960, 90),
        (4080, 100),
    ];
    if mv <= CURVE[0].0 {
        return 0;
    }
    for w in CURVE.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if mv <= v1 {
            return (p0 + (mv - v0) * (p1 - p0) / (v1 - v0)) as u8;
        }
    }
    100
}

/// Below this an e-paper refresh risks a brown-out half way: skip the update.
pub const MIN_UPDATE_MV: u32 = 3450;

pub fn too_low_to_update(mv: u32) -> bool {
    mv < MIN_UPDATE_MV
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_percent_follows_seeeds_calibration() {
        // Seeed's reTerminal E-series table (ESPHome cookbook, same ADC x2 measurement)
        assert_eq!(percent(3960), 90);
        assert_eq!(percent(3750), 50);
        assert_eq!(percent(3490), 20);
        assert_eq!(percent(3270), 0);
        // a full battery reads 4.082-4.088 V on this board after hours on the
        // charger: that is 100 %
        assert_eq!(percent(4082), 100);
        assert_eq!(percent(4020), 95);
        assert_eq!(percent(4200), 100);
        assert_eq!(percent(5000), 100);
        assert_eq!(percent(3000), 0);
        let mut last = 0;
        for mv in (3000..=4300).step_by(10) {
            let p = percent(mv);
            assert!(p >= last, "not monotonic at {mv}");
            last = p;
        }
    }

    #[test]
    fn too_low_to_update() {
        assert!(super::too_low_to_update(3449));
        assert!(!super::too_low_to_update(3450));
    }
}
