//! PCF8563 real-time clock registers 0x02..=0x08 (BCD, kept in UTC).

use crate::date::Date;

/// Registers for a Unix time: [sec, min, hour, day, weekday (0 = Sunday), month, year].
pub fn encode(unix: i64) -> [u8; 7] {
    let d = Date::from_days(unix.div_euclid(86_400));
    let s = unix.rem_euclid(86_400);
    let bcd = |v: i64| ((v / 10) << 4 | v % 10) as u8;
    [
        bcd(s % 60),
        bcd(s / 60 % 60),
        bcd(s / 3600),
        bcd(i64::from(d.day)),
        (d.weekday() + 1) % 7,
        bcd(i64::from(d.month)), // century bit 0 = 20xx
        bcd(i64::from(d.year % 100)),
    ]
}

/// Unix time from the registers; `None` if the clock lost power (VL bit) or is invalid.
pub fn decode(regs: [u8; 7]) -> Option<i64> {
    if regs[0] & 0x80 != 0 {
        return None;
    }
    let bin = |v: u8| -> Option<i64> {
        let (hi, lo) = (v >> 4, v & 0x0F);
        (hi <= 9 && lo <= 9).then(|| i64::from(hi * 10 + lo))
    };
    let (sec, min, hour) = (bin(regs[0] & 0x7F)?, bin(regs[1] & 0x7F)?, bin(regs[2] & 0x3F)?);
    let (day, month, year) = (bin(regs[3] & 0x3F)?, bin(regs[5] & 0x1F)?, bin(regs[6])?);
    if sec > 59 || min > 59 || hour > 23 || !(1..=12).contains(&month) || day < 1 || day > 31 {
        return None;
    }
    let date = Date { year: 2000 + year as i32, month: month as u8, day: day as u8 };
    if Date::from_days(date.days_since_epoch()) != date {
        return None; // e.g. 31 February
    }
    Some(date.days_since_epoch() * 86_400 + hour * 3600 + min * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u8, d: u8, h: i64) -> i64 {
        Date { year: y, month: m, day: d }.days_since_epoch() * 86_400 + h * 3600
    }

    #[test]
    fn bcd_round_trip() {
        let t = utc(2026, 11, 25, 4);
        assert_eq!(encode(t), [0x00, 0x00, 0x04, 0x25, 0x03, 0x11, 0x26]);
        assert_eq!(decode(encode(t)), Some(t));
        let t = utc(2031, 2, 28, 23) + 59 * 60 + 58;
        assert_eq!(decode(encode(t)), Some(t));
    }

    #[test]
    fn low_voltage_flag_means_unknown() {
        let mut r = encode(utc(2026, 11, 25, 4));
        r[0] |= 0x80;
        assert_eq!(decode(r), None);
        assert_eq!(decode([0x00, 0x00, 0x04, 0x32, 0x03, 0x11, 0x26]), None, "day 32");
        assert_eq!(decode([0x00, 0x00, 0x04, 0x31, 0x03, 0x02, 0x26]), None, "31 February");
    }
}
