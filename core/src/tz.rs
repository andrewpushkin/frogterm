//! Israel local time (Asia/Jerusalem) without a time-zone database.
//!
//! Standard time UTC+2; daylight time UTC+3 from the Friday before the last
//! Sunday of March, 02:00 local, until the last Sunday of October, 02:00 local
//! (POSIX `IST-2IDT,M3.4.4/26,M10.5.0`, the rule since 2013).

use crate::date::Date;

/// UTC offset in seconds at the given Unix time.
pub fn israel_offset(unix_secs: i64) -> i32 {
    const STD: i32 = 2 * 3600;
    const DST: i32 = 3 * 3600;
    let year = Date::from_days(unix_secs.div_euclid(86_400)).year;
    // last Sunday of March / October
    let last_sunday = |month: u8| {
        let last = Date { year, month, day: 31 }.days_since_epoch();
        last - i64::from((Date::from_days(last).weekday() + 1) % 7)
    };
    // Friday before the last Sunday of March, 02:00 standard time
    let start = (last_sunday(3) - 2) * 86_400 + 2 * 3600 - i64::from(STD);
    // last Sunday of October, 02:00 daylight time
    let end = last_sunday(10) * 86_400 + 2 * 3600 - i64::from(DST);
    if (start..end).contains(&unix_secs) {
        DST
    } else {
        STD
    }
}

/// Local date and seconds since local midnight for a Unix time.
pub fn to_local(unix_secs: i64) -> (Date, u32) {
    let local = unix_secs + i64::from(israel_offset(unix_secs));
    (Date::from_days(local.div_euclid(86_400)), local.rem_euclid(86_400) as u32)
}

/// Unix time of a local wall-clock time.
pub fn from_local(date: Date, secs_of_day: u32) -> i64 {
    let wall = date.days_since_epoch() * 86_400 + i64::from(secs_of_day);
    // guess with standard time, then correct with the offset valid at that instant
    let guess = wall - 2 * 3600;
    wall - i64::from(israel_offset(guess))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u8, d: u8, h: i64, min: i64) -> i64 {
        Date { year: y, month: m, day: d }.days_since_epoch() * 86_400 + h * 3600 + min * 60
    }

    #[test]
    fn winter_is_utc_plus_2_and_summer_utc_plus_3() {
        assert_eq!(israel_offset(utc(2026, 11, 25, 12, 0)), 7200);
        assert_eq!(israel_offset(utc(2026, 7, 1, 12, 0)), 10_800);
    }

    #[test]
    fn dst_starts_friday_before_last_sunday_of_march_at_2_local() {
        // 2026: last Sunday of March = 29th, so Friday 27 March 02:00 IST = 00:00 UTC
        assert_eq!(israel_offset(utc(2026, 3, 26, 23, 59)), 7200);
        assert_eq!(israel_offset(utc(2026, 3, 27, 0, 0)), 10_800);
        // 2027: last Sunday of March = 28th -> Friday 26 March
        assert_eq!(israel_offset(utc(2027, 3, 25, 23, 59)), 7200);
        assert_eq!(israel_offset(utc(2027, 3, 26, 0, 0)), 10_800);
    }

    #[test]
    fn dst_ends_last_sunday_of_october_at_2_local() {
        // 2026: last Sunday of October = 25th, 02:00 IDT = 24 Oct 23:00 UTC
        assert_eq!(israel_offset(utc(2026, 10, 24, 22, 59)), 10_800);
        assert_eq!(israel_offset(utc(2026, 10, 24, 23, 0)), 7200);
    }

    #[test]
    fn local_conversions_round_trip() {
        let t = utc(2026, 11, 25, 11, 0);
        assert_eq!(to_local(t), (Date { year: 2026, month: 11, day: 25 }, 13 * 3600));
        assert_eq!(from_local(Date { year: 2026, month: 11, day: 25 }, 13 * 3600), t);
        let summer = utc(2026, 8, 12, 5, 30);
        assert_eq!(to_local(summer), (Date { year: 2026, month: 8, day: 12 }, 8 * 3600 + 30 * 60));
        assert_eq!(from_local(Date { year: 2026, month: 8, day: 12 }, 8 * 3600 + 30 * 60), summer);
        // late evening UTC is already tomorrow in Israel
        assert_eq!(to_local(utc(2026, 11, 24, 23, 30)).0, Date { year: 2026, month: 11, day: 25 });
    }
}
