//! Israel local time (Asia/Jerusalem) without a time-zone database.
//!
//! Standard time UTC+2; daylight time UTC+3 from the Friday before the last
//! Sunday of March, 02:00 local, until the last Sunday of October, 02:00 local
//! (POSIX `IST-2IDT,M3.4.4/26,M10.5.0`, the rule since 2013).

use crate::date::Date;

/// Israel (Asia/Jerusalem) in POSIX form; the default `timezone`.
pub const ISRAEL: &str = "IST-2IDT,M3.4.4/26,M10.5.0";

/// A time zone from a POSIX TZ string like `IST-2IDT,M3.4.4/26,M10.5.0`
/// (name, UTC offset with inverted sign, optional DST name/offset and
/// `Mmonth.week.weekday[/time]` start and end rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    /// Standard time offset from UTC, seconds (east positive).
    pub std_offset: i32,
    pub dst: Option<Dst>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dst {
    pub offset: i32,
    /// Starts at this local standard time.
    pub start: Rule,
    /// Ends at this local daylight time.
    pub end: Rule,
}

/// `Mm.w.d/t`: weekday `d` (0 = Sunday) of week `w` (5 = last) of month `m`,
/// at `t` seconds after local midnight (may exceed 24 h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub month: u8,
    pub week: u8,
    pub weekday: u8,
    pub secs: i32,
}

impl Zone {
    pub fn parse(posix: &str) -> Option<Zone> {
        let (_, rest) = take_name(posix)?;
        let (std, rest) = take_time(rest)?;
        let std_offset = -std;
        if rest.is_empty() {
            return Some(Zone { std_offset, dst: None });
        }
        let (_, rest) = take_name(rest)?;
        let (offset, rest) = match take_time(rest) {
            Some((t, r)) => (-t, r),
            None => (std_offset + 3600, rest),
        };
        let rest = rest.strip_prefix(',')?;
        let (start, rest) = take_rule(rest)?;
        let (end, rest) = take_rule(rest.strip_prefix(',')?)?;
        rest.is_empty().then_some(Zone { std_offset, dst: Some(Dst { offset, start, end }) })
    }

    /// UTC offset in seconds at the given Unix time.
    pub fn offset(&self, unix_secs: i64) -> i32 {
        let Some(dst) = self.dst else { return self.std_offset };
        let year = Date::from_days((unix_secs + i64::from(self.std_offset)).div_euclid(86_400)).year;
        let start = rule_unix(year, dst.start) - i64::from(self.std_offset);
        let end = rule_unix(year, dst.end) - i64::from(dst.offset);
        let in_dst = if start < end { (start..end).contains(&unix_secs) } else { !(end..start).contains(&unix_secs) };
        if in_dst {
            dst.offset
        } else {
            self.std_offset
        }
    }
}

/// Zone name: 3+ letters, or `<...>`.
fn take_name(s: &str) -> Option<(&str, &str)> {
    if let Some(r) = s.strip_prefix('<') {
        let end = r.find('>')?;
        return Some((&r[..end], &r[end + 1..]));
    }
    let n = s.bytes().take_while(u8::is_ascii_alphabetic).count();
    (n >= 3).then(|| (&s[..n], &s[n..]))
}

/// `[+-]hh[:mm[:ss]]` in seconds.
fn take_time(s: &str) -> Option<(i32, &str)> {
    let (sign, s) = match s.as_bytes().first()? {
        b'-' => (-1, &s[1..]),
        b'+' => (1, &s[1..]),
        _ => (1, s),
    };
    let mut total = 0;
    let mut rest = s;
    for (i, unit) in [3600, 60, 1].into_iter().enumerate() {
        if i > 0 {
            match rest.strip_prefix(':') {
                Some(r) => rest = r,
                None => break,
            }
        }
        let n = rest.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        total += rest[..n].parse::<i32>().ok()? * unit;
        rest = &rest[n..];
    }
    Some((sign * total, rest))
}

/// `Mm.w.d[/time]`.
fn take_rule(s: &str) -> Option<(Rule, &str)> {
    let s = s.strip_prefix('M')?;
    let num = |s: &str| -> Option<(u8, usize)> {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        Some((s[..n].parse().ok()?, n))
    };
    let (month, n) = num(s)?;
    let s = s[n..].strip_prefix('.')?;
    let (week, n) = num(s)?;
    let s = s[n..].strip_prefix('.')?;
    let (weekday, n) = num(s)?;
    let mut rest = &s[n..];
    let mut secs = 2 * 3600;
    if let Some(r) = rest.strip_prefix('/') {
        let (t, r) = take_time(r)?;
        secs = t;
        rest = r;
    }
    let ok = (1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6;
    ok.then_some((Rule { month, week, weekday, secs }, rest))
}

/// Unix time of a rule's local wall-clock moment in `year` (offset not applied).
fn rule_unix(year: i32, r: Rule) -> i64 {
    let first = Date { year, month: r.month, day: 1 }.days_since_epoch();
    let first_wd = i64::from((Date::from_days(first).weekday() + 1) % 7); // 0 = Sunday
    let mut day = first + (i64::from(r.weekday) - first_wd).rem_euclid(7) + (i64::from(r.week) - 1) * 7;
    while Date::from_days(day).month != r.month {
        day -= 7; // "week 5" = last such weekday
    }
    day * 86_400 + i64::from(r.secs)
}

static ZONE: std::sync::OnceLock<Zone> = std::sync::OnceLock::new();

/// Sets the local time zone once at start-up (default: Israel).
pub fn set_zone(zone: Zone) {
    let _ = ZONE.set(zone);
}

fn zone() -> Zone {
    *ZONE.get_or_init(|| Zone::parse(ISRAEL).expect("built-in zone is valid"))
}

/// Israel's rule written out by hand (reference for the `Zone` tests).
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
    let local = unix_secs + i64::from(zone().offset(unix_secs));
    (Date::from_days(local.div_euclid(86_400)), local.rem_euclid(86_400) as u32)
}

/// Unix time of a local wall-clock time.
pub fn from_local(date: Date, secs_of_day: u32) -> i64 {
    let wall = date.days_since_epoch() * 86_400 + i64::from(secs_of_day);
    // guess with standard time, then correct with the offset valid at that instant
    let z = zone();
    let guess = wall - i64::from(z.std_offset);
    wall - i64::from(z.offset(guess))
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
    fn israel_posix_string_parses_to_its_rules() {
        let z = Zone::parse(ISRAEL).unwrap();
        assert_eq!(z.std_offset, 7200);
        let dst = z.dst.unwrap();
        assert_eq!(dst.offset, 10_800);
        assert_eq!(dst.start, Rule { month: 3, week: 4, weekday: 4, secs: 26 * 3600 });
        assert_eq!(dst.end, Rule { month: 10, week: 5, weekday: 0, secs: 2 * 3600 });
    }

    #[test]
    fn israel_zone_matches_the_built_in_rules() {
        let z = Zone::parse(ISRAEL).unwrap();
        for (y, m, d, h, min) in [
            (2026, 3, 26, 23, 59),
            (2026, 3, 27, 0, 0),
            (2026, 10, 24, 22, 59),
            (2026, 10, 24, 23, 0),
            (2027, 3, 26, 0, 0),
            (2026, 7, 1, 12, 0),
        ] {
            let t = utc(y, m, d, h, min);
            assert_eq!(z.offset(t), israel_offset(t), "{y}-{m}-{d} {h}:{min}");
        }
    }

    #[test]
    fn central_europe_and_japan() {
        let cet = Zone::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        // 2026: last Sunday of March = 29th, 02:00 CET = 01:00 UTC
        assert_eq!(cet.offset(utc(2026, 3, 29, 0, 59)), 3600);
        assert_eq!(cet.offset(utc(2026, 3, 29, 1, 0)), 7200);
        // last Sunday of October = 25th, 03:00 CEST = 01:00 UTC
        assert_eq!(cet.offset(utc(2026, 10, 25, 0, 59)), 7200);
        assert_eq!(cet.offset(utc(2026, 10, 25, 1, 0)), 3600);
        let jst = Zone::parse("JST-9").unwrap();
        assert_eq!(jst.dst, None);
        assert_eq!(jst.offset(utc(2026, 7, 1, 0, 0)), 9 * 3600);
        assert_eq!(Zone::parse("UTC0").unwrap().offset(0), 0);
    }

    #[test]
    fn bad_zone_strings_are_rejected() {
        for bad in ["", "Asia/Jerusalem", "IST", "IST-2IDT,M13.1.0,M10.5.0", "IST-2IDT,M3.4.4"] {
            assert_eq!(Zone::parse(bad), None, "{bad:?}");
        }
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
