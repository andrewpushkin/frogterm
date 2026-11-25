//! Civil dates without a date library (works the same on the ESP32).

/// A calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// Local date of a Unix time for a given UTC offset.
    pub fn from_unix(unix_secs: i64, utc_offset_secs: i32) -> Date {
        Date::from_days((unix_secs + i64::from(utc_offset_secs)).div_euclid(86_400))
    }

    /// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    pub fn days_since_epoch(self) -> i64 {
        let m = i64::from(self.month);
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// Inverse of `days_since_epoch` (civil_from_days).
    pub fn from_days(days: i64) -> Date {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Date { year, month, day }
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(self) -> u8 {
        // 1970-01-01 was a Thursday (3)
        (self.days_since_epoch() + 3).rem_euclid(7) as u8
    }

    /// e.g. "Wed 25 Nov".
    pub fn label(self) -> String {
        const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        format!("{} {} {}", DAYS[usize::from(self.weekday())], self.day, self.month_name())
    }

    /// When a photo was taken, e.g. "7 Nov 2026".
    pub fn photo_label(self) -> String {
        format!("{} {} {}", self.day, self.month_name(), self.year)
    }

    fn month_name(self) -> &'static str {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        MONTHS[usize::from(self.month.clamp(1, 12) - 1)]
    }

    /// "2026-11-07".
    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Inverse of `iso`; `None` for anything that is not a real date.
    pub fn parse_iso(s: &str) -> Option<Date> {
        let mut f = s.splitn(3, '-');
        let year = f.next()?.parse().ok()?;
        let month = f.next()?.parse().ok()?;
        let day = f.next()?.parse().ok()?;
        let date = Date { year, month, day };
        // round trip rejects 13th months, 32nd days, 30 Feb …
        (Date::from_days(date.days_since_epoch()) == date && (1..=12).contains(&month)).then_some(date)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(year: i32, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn epoch_and_known_days() {
        assert_eq!(Date::from_days(0), d(1970, 1, 1));
        assert_eq!(d(2026, 11, 25).days_since_epoch(), 20_782);
        assert_eq!(Date::from_days(20_782), d(2026, 11, 25));
        assert_eq!(Date::from_days(d(2028, 2, 29).days_since_epoch()), d(2028, 2, 29));
    }

    #[test]
    fn local_date_uses_the_offset() {
        // 2026-11-24 23:30 UTC is already the 25th in Israel (UTC+2)
        let t = d(2026, 11, 24).days_since_epoch() * 86_400 + 23 * 3600 + 30 * 60;
        assert_eq!(Date::from_unix(t, 0), d(2026, 11, 24));
        assert_eq!(Date::from_unix(t, 7200), d(2026, 11, 25));
    }

    #[test]
    fn weekday_and_label() {
        assert_eq!(d(2026, 11, 25).weekday(), 2, "Wednesday");
        assert_eq!(d(1970, 1, 1).weekday(), 3, "Thursday");
        assert_eq!(d(2026, 11, 25).label(), "Wed 25 Nov");
        assert_eq!(d(2027, 1, 3).label(), "Sun 3 Jan");
    }

    #[test]
    fn photo_label_has_the_year() {
        assert_eq!(d(2026, 11, 7).photo_label(), "7 Nov 2026");
        assert_eq!(d(2019, 1, 23).photo_label(), "23 Jan 2019");
    }

    #[test]
    fn iso_round_trips() {
        assert_eq!(d(2026, 3, 7).iso(), "2026-03-07");
        assert_eq!(Date::parse_iso("2026-03-07"), Some(d(2026, 3, 7)));
        for bad in ["", "-", "2026-13-01", "2026-00-10", "2026-02-32", "2026-1-1x", "today"] {
            assert_eq!(Date::parse_iso(bad), None, "{bad}");
        }
    }
}
