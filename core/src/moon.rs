//! Moon phase from the date: days since a known new moon, modulo the
//! synodic month. Good to a few hours - plenty for an icon.

/// Phase in `0.0..1.0`: 0 = new moon, 0.25 = first quarter, 0.5 = full, 0.75 = last quarter.
pub fn phase(unix_secs: i64) -> f32 {
    /// A new moon: 2000-01-06 18:14 UTC.
    const NEW_MOON: f64 = 947_182_440.0;
    /// Mean synodic month in seconds (29.530588853 days).
    const SYNODIC: f64 = 29.530_588_853 * 86_400.0;
    let p = ((unix_secs as f64 - NEW_MOON) / SYNODIC).rem_euclid(1.0) as f32;
    if p >= 1.0 { 0.0 } else { p }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::Date;

    fn utc(y: i32, m: u8, d: u8, h: i64, min: i64) -> i64 {
        Date { year: y, month: m, day: d }.days_since_epoch() * 86_400 + h * 3600 + min * 60
    }

    /// Distance between phases on the circle.
    fn off(a: f32, b: f32) -> f32 {
        let d = (a - b).abs();
        d.min(1.0 - d)
    }

    #[test]
    fn known_new_and_full_moons() {
        // published times (UTC)
        assert!(off(phase(utc(2024, 1, 11, 11, 57)), 0.0) < 0.02, "new moon 2024-01-11");
        assert!(off(phase(utc(2024, 1, 25, 17, 54)), 0.5) < 0.02, "full moon 2024-01-25");
        assert!(off(phase(utc(2026, 11, 24, 14, 53)), 0.5) < 0.02, "full moon 2026-11-24");
        assert!(off(phase(utc(2026, 12, 9, 0, 52)), 0.0) < 0.02, "new moon 2026-12-09");
    }

    #[test]
    fn phase_is_in_range() {
        for d in 0..400 {
            let p = phase(utc(2026, 1, 1, 0, 0) + d * 86_400);
            assert!((0.0..1.0).contains(&p), "{p}");
        }
    }
}
