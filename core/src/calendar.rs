//! Today's events from Google Calendar's "secret address in iCal format".
//!
//! The iCal file holds the calendar's whole history (~100 KB for the owner's
//! Family calendar), so it is read as a stream: bytes are split into lines,
//! folded lines are joined, and only one VEVENT is held at a time.

use std::collections::HashSet;

use crate::date::Date;
use crate::tz;

/// One event as shown on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayEvent {
    /// Local start time `(hour, minute)`; `None` = all-day.
    pub start: Option<(u8, u8)>,
    /// Local end time today; `None` = all-day or ends after today.
    pub end: Option<(u8, u8)>,
    pub title: String,
}

/// Streaming reader of one iCal file, collecting the events of `day` (local).
pub struct IcsScanner {
    day: Date,
    /// Bytes of the current physical line (not yet terminated).
    partial: Vec<u8>,
    /// Current logical line, still open for folded continuations.
    pending: Option<String>,
    /// The VEVENT being read, if inside one.
    event: Option<VEvent>,
    /// Occurrences touching the day.
    found: Vec<Found>,
    /// (uid, original local date) replaced by a RECURRENCE-ID override.
    overridden: HashSet<(String, Date)>,
}

struct Found {
    uid: String,
    /// Local date the occurrence originally belongs to (key for overrides).
    date: Date,
    /// Comes from a RECURRENCE-ID override (never hidden by overrides itself).
    is_override: bool,
    event: DayEvent,
}

/// A date or date-time value as written in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    date: Date,
    /// Seconds since midnight; `None` for a DATE value (all-day).
    secs: Option<u32>,
    /// `...Z` (UTC) rather than local/TZID wall-clock time.
    utc: bool,
}

impl Stamp {
    fn parse(params: &str, value: &str) -> Option<Stamp> {
        let v = value.trim();
        let num = |r: std::ops::Range<usize>| v.get(r)?.parse::<u32>().ok();
        let date = Date { year: num(0..4)? as i32, month: num(4..6)? as u8, day: num(6..8)? as u8 };
        if params.contains("VALUE=DATE") && !params.contains("VALUE=DATE-TIME") || v.len() == 8 {
            return Some(Stamp { date, secs: None, utc: false });
        }
        if v.as_bytes().get(8) != Some(&b'T') {
            return None;
        }
        let secs = num(9..11)? * 3600 + num(11..13)? * 60 + num(13..15)?;
        Some(Stamp { date, secs: Some(secs), utc: v.ends_with('Z') })
    }

    /// Local (Israel) date and time of day. TZID values are taken as local wall-clock time.
    fn local(self) -> (Date, Option<u32>) {
        match self.secs {
            Some(s) if self.utc => {
                let (d, t) = tz::to_local(self.date.days_since_epoch() * 86_400 + i64::from(s));
                (d, Some(t))
            }
            secs => (self.date, secs),
        }
    }
}

impl Stamp {
    /// Unix time; local values are taken as Israel wall-clock time.
    fn unix(self) -> i64 {
        let secs = self.secs.unwrap_or(0);
        if self.utc {
            self.date.days_since_epoch() * 86_400 + i64::from(secs)
        } else {
            tz::from_local(self.date, secs)
        }
    }

    /// The same kind of value moved by `secs` seconds (or whole days for DATE values),
    /// counted in the value's own frame (UTC or wall clock).
    fn shifted(self, secs: i64) -> Stamp {
        let base = self.date.days_since_epoch() * 86_400 + i64::from(self.secs.unwrap_or(0)) + secs;
        Stamp {
            date: Date::from_days(base.div_euclid(86_400)),
            secs: self.secs.map(|_| base.rem_euclid(86_400) as u32),
            utc: self.utc,
        }
    }

    fn frame_secs(self) -> i64 {
        self.date.days_since_epoch() * 86_400 + i64::from(self.secs.unwrap_or(0))
    }
}

#[derive(Debug, Default)]
struct VEvent {
    uid: String,
    summary: String,
    start: Option<Stamp>,
    end: Option<Stamp>,
    rule: Option<Rule>,
    exdates: Vec<Stamp>,
    recurrence_id: Option<Stamp>,
    cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// The parts of RRULE that Google Calendar writes.
#[derive(Debug, Clone)]
struct Rule {
    freq: Freq,
    interval: i64,
    until: Option<Stamp>,
    count: Option<u32>,
    /// (ordinal, weekday 0=Mon); ordinal 0 = every such weekday.
    by_day: Vec<(i8, u8)>,
    by_month_day: Vec<i8>,
    by_month: Vec<u8>,
    /// Week start weekday (0=Mon) for INTERVAL>1 weekly rules.
    wkst: u8,
}

impl Rule {
    fn parse(value: &str) -> Option<Rule> {
        let mut r = Rule {
            freq: Freq::Daily,
            interval: 1,
            until: None,
            count: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            wkst: 0,
        };
        let mut freq = None;
        for part in value.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            match k {
                "FREQ" => {
                    freq = match v {
                        "DAILY" => Some(Freq::Daily),
                        "WEEKLY" => Some(Freq::Weekly),
                        "MONTHLY" => Some(Freq::Monthly),
                        "YEARLY" => Some(Freq::Yearly),
                        _ => None,
                    }
                }
                "INTERVAL" => r.interval = v.parse().ok().filter(|&i: &i64| i > 0)?,
                "UNTIL" => r.until = Stamp::parse("", v),
                "COUNT" => r.count = v.parse().ok(),
                "BYDAY" => r.by_day = v.split(',').filter_map(parse_byday).collect(),
                "BYMONTHDAY" => r.by_month_day = v.split(',').filter_map(|d| d.parse().ok()).collect(),
                "BYMONTH" => r.by_month = v.split(',').filter_map(|m| m.parse().ok()).collect(),
                "WKST" => r.wkst = weekday_code(v)?,
                _ => {}
            }
        }
        r.freq = freq?;
        Some(r)
    }

    /// Whether the series starting at `first` has an occurrence on date `d`
    /// (dates in the series' own frame), ignoring UNTIL and COUNT.
    fn matches(&self, first: Date, d: Date) -> bool {
        if d < first {
            return false;
        }
        let (fd, dd) = (first.days_since_epoch(), d.days_since_epoch());
        let months = (i64::from(d.year) * 12 + i64::from(d.month)) - (i64::from(first.year) * 12 + i64::from(first.month));
        match self.freq {
            Freq::Daily => (dd - fd) % self.interval == 0 && self.weekday_ok(d),
            Freq::Weekly => {
                let week_start = |x: i64| x - i64::from((Date::from_days(x).weekday() + 7 - self.wkst) % 7);
                let weeks = (week_start(dd) - week_start(fd)) / 7;
                let days: Vec<u8> = if self.by_day.is_empty() {
                    vec![first.weekday()]
                } else {
                    self.by_day.iter().map(|&(_, w)| w).collect()
                };
                weeks % self.interval == 0 && days.contains(&d.weekday())
            }
            Freq::Monthly => months % self.interval == 0 && self.day_in_month_ok(first, d),
            Freq::Yearly => {
                let month_ok = if self.by_month.is_empty() { d.month == first.month } else { self.by_month.contains(&d.month) };
                (i64::from(d.year) - i64::from(first.year)) % self.interval == 0 && month_ok && self.day_in_month_ok(first, d)
            }
        }
    }

    fn weekday_ok(&self, d: Date) -> bool {
        self.by_day.is_empty() || self.by_day.iter().any(|&(_, w)| w == d.weekday())
    }

    /// BYDAY (with ordinals) / BYMONTHDAY / the start's day of month.
    fn day_in_month_ok(&self, first: Date, d: Date) -> bool {
        let dim = days_in_month(d.year, d.month);
        if !self.by_day.is_empty() {
            return self.by_day.iter().any(|&(n, w)| {
                w == d.weekday()
                    && match n {
                        0 => true,
                        n if n > 0 => (d.day - 1) / 7 + 1 == n as u8,
                        n => (dim - d.day) / 7 + 1 == n.unsigned_abs(),
                    }
            });
        }
        if !self.by_month_day.is_empty() {
            return self.by_month_day.iter().any(|&m| {
                if m > 0 { d.day == m as u8 } else { i16::from(d.day) == i16::from(dim) + 1 + i16::from(m) }
            });
        }
        d.day == first.day
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    Date::from_days(Date { year: ny, month: nm, day: 1 }.days_since_epoch() - 1).day
}

fn weekday_code(s: &str) -> Option<u8> {
    ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].iter().position(|&c| c == s).map(|p| p as u8)
}

/// "WE", "2TU", "-1FR".
fn parse_byday(s: &str) -> Option<(i8, u8)> {
    let split = s.len().checked_sub(2)?;
    let n = if split == 0 { 0 } else { s[..split].parse().ok()? };
    Some((n, weekday_code(&s[split..])?))
}

impl IcsScanner {
    pub fn new(day: Date) -> Self {
        Self { day, partial: Vec::new(), pending: None, event: None, found: Vec::new(), overridden: HashSet::new() }
    }

    /// Feed any chunk of the file (chunks may split lines or UTF-8 characters).
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                let mut line = std::mem::take(&mut self.partial);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.physical_line(String::from_utf8_lossy(&line).into_owned());
            } else {
                self.partial.push(b);
            }
        }
    }

    /// Events of the day: all-day first, then by start time.
    pub fn finish(mut self) -> Vec<DayEvent> {
        if !self.partial.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned();
            self.physical_line(line);
        }
        if let Some(l) = self.pending.take() {
            self.logical_line(&l);
        }
        let overridden = self.overridden;
        let mut events: Vec<DayEvent> = self
            .found
            .into_iter()
            .filter(|f| f.is_override || !overridden.contains(&(f.uid.clone(), f.date)))
            .map(|f| f.event)
            .collect();
        sort_events(&mut events);
        events
    }

    fn physical_line(&mut self, line: String) {
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(p) = self.pending.as_mut() {
                p.push_str(rest);
            }
            return;
        }
        if let Some(p) = self.pending.replace(line) {
            self.logical_line(&p);
        }
    }

    fn logical_line(&mut self, line: &str) {
        let Some((head, value)) = line.split_once(':') else { return };
        let (name, params) = head.split_once(';').unwrap_or((head, ""));
        match (name, value) {
            ("BEGIN", "VEVENT") => self.event = Some(VEvent::default()),
            ("END", "VEVENT") => {
                if let Some(e) = self.event.take() {
                    self.add(e);
                }
            }
            _ => {
                let Some(e) = self.event.as_mut() else { return };
                match name {
                    "UID" => e.uid = value.to_string(),
                    "SUMMARY" => e.summary = unescape(value),
                    "DTSTART" => e.start = Stamp::parse(params, value),
                    "DTEND" => e.end = Stamp::parse(params, value),
                    "RRULE" => e.rule = Rule::parse(value),
                    "EXDATE" => e.exdates.extend(value.split(',').filter_map(|v| Stamp::parse(params, v))),
                    "RECURRENCE-ID" => e.recurrence_id = Stamp::parse(params, value),
                    "STATUS" => e.cancelled = value == "CANCELLED",
                    _ => {}
                }
            }
        }
    }

    fn add(&mut self, e: VEvent) {
        let Some(start) = e.start else { return };
        let is_override = e.recurrence_id.is_some();
        if let Some(rid) = e.recurrence_id {
            // this VEVENT replaces (or cancels) one occurrence of its series
            self.overridden.insert((e.uid.clone(), rid.local().0));
        }
        if e.cancelled {
            return;
        }
        let Some(rule) = &e.rule else {
            let (sd, st) = start.local();
            if let Some(event) = self.on_day(sd, st, e.end.map(Stamp::local), &e.summary) {
                self.found.push(Found { uid: e.uid, date: sd, is_override, event });
            }
            return;
        };
        let duration = e.end.map_or(0, |end| end.frame_secs() - start.frame_secs());
        // Occurrences live in the series' own frame (UTC or wall clock); a UTC
        // occurrence on the day before/after can still land on our local day.
        let today = self.day.days_since_epoch();
        for frame_day in today - 1..=today + 1 {
            let d = Date::from_days(frame_day);
            if !rule.matches(start.date, d) {
                continue;
            }
            let occ = Stamp { date: d, ..start };
            if rule.until.is_some_and(|u| occ.unix() > u.unix()) {
                continue;
            }
            let excluded = e.exdates.iter().any(|x| match (x.secs, occ.secs) {
                (None, _) | (_, None) => x.local().0 == occ.local().0,
                _ => x.unix() == occ.unix(),
            });
            if excluded {
                continue;
            }
            if let Some(count) = rule.count {
                let n = (start.date.days_since_epoch()..=frame_day)
                    .filter(|&x| rule.matches(start.date, Date::from_days(x)))
                    .count();
                if n > count as usize {
                    continue;
                }
            }
            let (od, ot) = occ.local();
            let end = (e.end.is_some()).then(|| occ.shifted(duration).local());
            if let Some(event) = self.on_day(od, ot, end, &e.summary) {
                self.found.push(Found { uid: e.uid.clone(), date: od, is_override: false, event });
            }
        }
    }

    /// The occurrence starting at local `(sd, st)` as seen on `self.day`, if it touches it.
    fn on_day(&self, sd: Date, st: Option<u32>, end: Option<(Date, Option<u32>)>, summary: &str) -> Option<DayEvent> {
        let day = self.day;
        let title = clean_title(summary);
        match st {
            None => {
                // all-day: DTEND is exclusive; missing or bad end = one day
                let ed = end.map(|(d, _)| d).filter(|&d| d > sd).unwrap_or(Date::from_days(sd.days_since_epoch() + 1));
                (sd <= day && day < ed).then_some(DayEvent { start: None, end: None, title })
            }
            Some(t) => {
                let (ed, et) = end.map_or((sd, Some(t)), |(d, s)| (d, s));
                let hm = |s: u32| ((s / 3600) as u8, (s % 3600 / 60) as u8);
                if sd == day {
                    let end = (ed == day).then(|| et.map(hm)).flatten();
                    Some(DayEvent { start: Some(hm(t)), end, title })
                } else if sd < day && (ed > day || (ed == day && et.unwrap_or(0) > 0)) {
                    // started earlier, still running today: show like an all-day item
                    Some(DayEvent { start: None, end: None, title })
                } else {
                    None
                }
            }
        }
    }
}

/// iCal text escapes: `\,` `\;` `\\` and `\n` (shown as a space).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Convenience: the events of `day` in a whole iCal text.
pub fn events_on(ics: &str, day: Date) -> Vec<DayEvent> {
    let mut s = IcsScanner::new(day);
    s.feed(ics.as_bytes());
    s.finish()
}

/// Screen order: all-day first, then by start time (also for merged calendars).
pub fn sort_events(events: &mut [DayEvent]) {
    events.sort_by(|a, b| (a.start.is_some(), a.start, &a.title).cmp(&(b.start.is_some(), b.start, &b.title)));
}

/// Events still worth showing at local time `now`: all-day ones and those not yet over.
pub fn remaining(events: Vec<DayEvent>, now: (u8, u8)) -> Vec<DayEvent> {
    events.into_iter().filter(|e| e.start.is_none() || e.end.is_none_or(|end| end > now)).collect()
}

/// What the events box shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agenda {
    /// `None` = calendar unavailable.
    pub events: Option<Vec<DayEvent>>,
    /// The events are tomorrow's (nothing is left today).
    pub tomorrow: bool,
}

/// Today's events not yet over; if none are left, tomorrow's.
pub fn agenda(today: Option<Vec<DayEvent>>, tomorrow: Option<Vec<DayEvent>>, now: (u8, u8)) -> Agenda {
    let Some(today) = today else {
        return Agenda { events: None, tomorrow: false };
    };
    let left = remaining(today, now);
    match tomorrow {
        Some(t) if left.is_empty() && !t.is_empty() => Agenda { events: Some(t), tomorrow: true },
        _ => Agenda { events: Some(left), tomorrow: false },
    }
}

/// Titles often repeat the time ("13:00Акробатика", "8:15 Собрание",
/// "Английский 13:00"); the time column already shows it, so drop a leading
/// or trailing `H:MM`/`HH:MM`.
pub fn clean_title(title: &str) -> String {
    let t = title.trim();
    let b = t.as_bytes();
    let is_time = |s: &[u8]| {
        let h = s.iter().take_while(|c| c.is_ascii_digit()).count();
        (1..=2).contains(&h) && s.len() == h + 3 && s[h] == b':' && s[h + 1..].iter().all(u8::is_ascii_digit)
    };
    // leading: up to the first non-time character
    let lead = b.iter().take_while(|c| c.is_ascii_digit()).count() + 3;
    if lead <= b.len() && is_time(&b[..lead]) && !b.get(lead).is_some_and(u8::is_ascii_digit) {
        let rest = t[lead..].trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    // trailing: after the last space
    if let Some(sp) = t.rfind(' ') {
        if is_time(&b[sp + 1..]) && !t[..sp].trim().is_empty() {
            return t[..sp].trim().to_string();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Date = Date { year: 2026, month: 11, day: 25 };

    /// A calendar the way Google writes it (CRLF, VTIMEZONE first).
    fn ics(events: &[&str]) -> String {
        let mut s = String::from(
            "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\n\
             BEGIN:VTIMEZONE\r\nTZID:Asia/Jerusalem\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0200\r\n\
             TZOFFSETTO:+0300\r\nDTSTART:19700327T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=4FR\r\n\
             END:DAYLIGHT\r\nEND:VTIMEZONE\r\n",
        );
        for e in events {
            s.push_str("BEGIN:VEVENT\r\n");
            for line in e.lines() {
                s.push_str(line);
                s.push_str("\r\n");
            }
            if !e.contains("STATUS:") {
                s.push_str("STATUS:CONFIRMED\r\n");
            }
            s.push_str("END:VEVENT\r\n");
        }
        s.push_str("END:VCALENDAR\r\n");
        s
    }

    fn ev(start: Option<(u8, u8)>, end: Option<(u8, u8)>, title: &str) -> DayEvent {
        DayEvent { start, end, title: title.into() }
    }

    #[test]
    fn utc_event_today_is_shown_in_local_time() {
        let cal = ics(&["DTSTART:20261125T110000Z\nDTEND:20261125T120000Z\nUID:a\nSUMMARY:13:00Дзюдо"]);
        assert_eq!(events_on(&cal, DAY), vec![ev(Some((13, 0)), Some((14, 0)), "Дзюдо")]);
    }

    #[test]
    fn local_time_event_keeps_its_wall_clock() {
        let cal = ics(&["DTSTART;TZID=Asia/Jerusalem:20261125T171500\nDTEND;TZID=Asia/Jerusalem:20261125T181500\nUID:b\nSUMMARY:Бассейн"]);
        assert_eq!(events_on(&cal, DAY), vec![ev(Some((17, 15)), Some((18, 15)), "Бассейн")]);
    }

    #[test]
    fn events_on_other_days_are_ignored() {
        let cal = ics(&[
            "DTSTART:20261126T110000Z\nDTEND:20261126T120000Z\nUID:c\nSUMMARY:tomorrow",
            "DTSTART;VALUE=DATE:20261124\nDTEND;VALUE=DATE:20261125\nUID:d\nSUMMARY:yesterday all day",
            // 23:30 UTC on the 25th is already the 26th in Israel
            "DTSTART:20261125T233000Z\nDTEND:20261126T000000Z\nUID:e\nSUMMARY:late",
        ]);
        assert_eq!(events_on(&cal, DAY), vec![]);
    }

    #[test]
    fn all_day_and_multi_day_events() {
        let cal = ics(&[
            "DTSTART;VALUE=DATE:20261125\nDTEND;VALUE=DATE:20261126\nUID:f\nSUMMARY:ДР у друга",
            "DTSTART;VALUE=DATE:20261124\nDTEND;VALUE=DATE:20261127\nUID:g\nSUMMARY:Trip",
        ]);
        let got = events_on(&cal, DAY);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.start.is_none() && e.end.is_none()));
    }

    #[test]
    fn timed_event_running_past_midnight_has_no_end_today() {
        let cal = ics(&["DTSTART;TZID=Asia/Jerusalem:20261125T220000\nDTEND;TZID=Asia/Jerusalem:20261126T010000\nUID:h\nSUMMARY:Night"]);
        assert_eq!(events_on(&cal, DAY), vec![ev(Some((22, 0)), None, "Night")]);
    }

    #[test]
    fn sorted_all_day_first_then_by_time() {
        let cal = ics(&[
            "DTSTART:20261125T150000Z\nDTEND:20261125T160000Z\nUID:1\nSUMMARY:late",
            "DTSTART;VALUE=DATE:20261125\nDTEND;VALUE=DATE:20261126\nUID:2\nSUMMARY:all day",
            "DTSTART:20261125T060000Z\nDTEND:20261125T070000Z\nUID:3\nSUMMARY:early",
        ]);
        let titles: Vec<String> = events_on(&cal, DAY).into_iter().map(|e| e.title).collect();
        assert_eq!(titles, ["all day", "early", "late"]);
    }

    #[test]
    fn folded_lines_and_escapes_are_decoded() {
        // a folded line continues after CRLF + one space
        let cal = ics(&["DTSTART:20261125T110000Z\nDTEND:20261125T120000Z\nUID:i\nSUMMARY:Иврит / Язык (שפה)\\, урок\\;\n  - учитель"]);
        assert_eq!(events_on(&cal, DAY)[0].title, "Иврит / Язык (שפה), урок; - учитель");
    }

    #[test]
    fn result_does_not_depend_on_chunk_boundaries() {
        let cal = ics(&[
            "DTSTART:20261125T110000Z\nDTEND:20261125T120000Z\nUID:a\nSUMMARY:13:00Дзюдо",
            "DTSTART;VALUE=DATE:20261125\nDTEND;VALUE=DATE:20261126\nUID:f\nSUMMARY:ДР у друга",
        ]);
        let want = events_on(&cal, DAY);
        for size in [1, 2, 3, 7, 64] {
            let mut s = IcsScanner::new(DAY);
            for chunk in cal.as_bytes().chunks(size) {
                s.feed(chunk);
            }
            assert_eq!(s.finish(), want, "chunk size {size}");
        }
    }

    #[test]
    fn remaining_drops_events_that_are_over() {
        let evs = vec![
            ev(None, None, "all day"),
            ev(Some((8, 0)), Some((9, 0)), "over"),
            ev(Some((12, 30)), Some((14, 0)), "running"),
            ev(Some((17, 0)), Some((18, 0)), "later"),
            ev(Some((22, 0)), None, "past midnight"),
        ];
        let titles: Vec<String> = remaining(evs, (13, 0)).into_iter().map(|e| e.title).collect();
        assert_eq!(titles, ["all day", "running", "later", "past midnight"]);
    }

    #[test]
    fn agenda_shows_today_while_events_are_left() {
        let today = vec![ev(Some((8, 0)), Some((9, 0)), "over"), ev(Some((17, 0)), Some((18, 0)), "later")];
        let tomorrow = vec![ev(Some((10, 0)), Some((11, 0)), "tomorrow")];
        let a = agenda(Some(today), Some(tomorrow), (12, 0));
        assert!(!a.tomorrow);
        assert_eq!(a.events.unwrap().iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["later"]);
    }

    #[test]
    fn agenda_shows_tomorrow_when_today_is_done() {
        let today = vec![ev(Some((8, 0)), Some((9, 0)), "over")];
        let tomorrow = vec![ev(Some((10, 0)), Some((11, 0)), "Дзюдо")];
        let a = agenda(Some(today), Some(tomorrow.clone()), (20, 0));
        assert_eq!(a, Agenda { events: Some(tomorrow), tomorrow: true });
        let a = agenda(Some(vec![]), Some(vec![ev(None, None, "ДР")]), (7, 0));
        assert!(a.tomorrow, "empty today");
    }

    #[test]
    fn agenda_with_nothing_today_or_tomorrow_or_no_calendar() {
        assert_eq!(agenda(Some(vec![]), Some(vec![]), (9, 0)), Agenda { events: Some(vec![]), tomorrow: false });
        assert_eq!(agenda(Some(vec![]), None, (9, 0)), Agenda { events: Some(vec![]), tomorrow: false });
        assert_eq!(agenda(None, None, (9, 0)), Agenda { events: None, tomorrow: false });
    }

    #[test]
    fn leading_time_is_removed_from_titles() {
        assert_eq!(clean_title("13:00Акробатика"), "Акробатика");
        assert_eq!(clean_title("17:30 Музыка "), "Музыка");
        assert_eq!(clean_title("8:15 Собрание в школе"), "Собрание в школе");
        assert_eq!(clean_title("Английский 13:00 "), "Английский");
    }

    #[test]
    fn other_titles_are_kept() {
        assert_eq!(clean_title("Музыка"), "Музыка");
        assert_eq!(clean_title("2026 plan"), "2026 plan");
        assert_eq!(clean_title("13:00"), "13:00", "a title that is only a time stays");
        assert_eq!(clean_title("1:2 score"), "1:2 score");
    }
    fn one(event: &str, day: Date) -> Vec<DayEvent> {
        events_on(&ics(&[event]), day)
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date { year: y, month: m, day: d }
    }

    #[test]
    fn weekly_without_byday_repeats_on_the_start_weekday_until_the_end() {
        // 2 Sep 2026 is a Wednesday, like 25 Nov
        let e = "DTSTART;TZID=Asia/Jerusalem:20260902T130000\nDTEND;TZID=Asia/Jerusalem:20260902T140000\nRRULE:FREQ=WEEKLY;WKST=MO;UNTIL=20270630T205959Z\nUID:judo\nSUMMARY:13:00Дзюдо";
        assert_eq!(one(e, DAY), vec![ev(Some((13, 0)), Some((14, 0)), "Дзюдо")]);
        assert_eq!(one(e, date(2026, 11, 26)), vec![], "Thursday");
        assert_eq!(one(e, date(2026, 8, 26)), vec![], "before the start");
        assert_eq!(one(e, date(2027, 7, 7)), vec![], "after UNTIL");
    }

    #[test]
    fn weekly_byday_list() {
        let e = "DTSTART;TZID=Asia/Jerusalem:20260907T080000\nDTEND;TZID=Asia/Jerusalem:20260907T084500\nRRULE:FREQ=WEEKLY;WKST=SU;INTERVAL=1;BYDAY=MO,WE\nUID:x\nSUMMARY:Иврит";
        assert_eq!(one(e, DAY).len(), 1, "Wednesday");
        assert_eq!(one(e, date(2026, 11, 23)).len(), 1, "Monday");
        assert_eq!(one(e, date(2026, 11, 24)).len(), 0, "Tuesday");
    }

    #[test]
    fn every_second_week() {
        // starts Wed 11 Nov: occurs 11 Nov, 25 Nov, not 18 Nov
        let e = "DTSTART;TZID=Asia/Jerusalem:20261111T100000\nDTEND;TZID=Asia/Jerusalem:20261111T110000\nRRULE:FREQ=WEEKLY;WKST=SU;INTERVAL=2;BYDAY=WE\nUID:x\nSUMMARY:biweekly";
        assert_eq!(one(e, DAY).len(), 1);
        assert_eq!(one(e, date(2026, 11, 18)).len(), 0);
    }

    #[test]
    fn daily_with_interval_and_count() {
        let e = "DTSTART;TZID=Asia/Jerusalem:20261121T090000\nDTEND;TZID=Asia/Jerusalem:20261121T091500\nRRULE:FREQ=DAILY;INTERVAL=2;COUNT=3\nUID:x\nSUMMARY:pills";
        assert_eq!(one(e, date(2026, 11, 23)).len(), 1, "2nd occurrence");
        assert_eq!(one(e, date(2026, 11, 24)).len(), 0, "off day");
        assert_eq!(one(e, DAY).len(), 1, "3rd occurrence");
        assert_eq!(one(e, date(2026, 11, 27)).len(), 0, "COUNT reached");
    }

    #[test]
    fn monthly_by_date_and_by_nth_weekday() {
        let by_date = "DTSTART;VALUE=DATE:20260125\nDTEND;VALUE=DATE:20260126\nRRULE:FREQ=MONTHLY\nUID:x\nSUMMARY:rent";
        assert_eq!(one(by_date, DAY).len(), 1);
        assert_eq!(one(by_date, date(2026, 11, 24)).len(), 0);
        let last_wed = "DTSTART;TZID=Asia/Jerusalem:20260128T190000\nDTEND;TZID=Asia/Jerusalem:20260128T200000\nRRULE:FREQ=MONTHLY;BYDAY=-1WE\nUID:y\nSUMMARY:club";
        assert_eq!(one(last_wed, DAY).len(), 1, "25 Nov is the last Wednesday");
        assert_eq!(one(last_wed, date(2026, 11, 18)).len(), 0);
        let fourth_wed = "DTSTART;TZID=Asia/Jerusalem:20260128T190000\nDTEND;TZID=Asia/Jerusalem:20260128T200000\nRRULE:FREQ=MONTHLY;BYDAY=4WE\nUID:z\nSUMMARY:club";
        assert_eq!(one(fourth_wed, DAY).len(), 1);
    }

    #[test]
    fn yearly_birthday() {
        let e = "DTSTART;VALUE=DATE:19851125\nDTEND;VALUE=DATE:19851126\nRRULE:FREQ=YEARLY\nUID:x\nSUMMARY:ДР у друга";
        assert_eq!(one(e, DAY), vec![ev(None, None, "ДР у друга")]);
        assert_eq!(one(e, date(2026, 11, 24)), vec![]);
    }

    #[test]
    fn utc_based_repeat_keeps_utc_time_across_dst() {
        // 05:30 UTC = 08:30 in summer, 07:30 in winter
        let e = "DTSTART:20260812T053000Z\nDTEND:20260812T063000Z\nRRULE:FREQ=WEEKLY;BYDAY=WE\nUID:x\nSUMMARY:utc";
        assert_eq!(one(e, DAY)[0].start, Some((7, 30)));
        assert_eq!(one(e, date(2026, 8, 19))[0].start, Some((8, 30)));
    }

    #[test]
    fn local_repeat_keeps_wall_clock_across_dst() {
        let e = "DTSTART;TZID=Asia/Jerusalem:20260812T083000\nDTEND;TZID=Asia/Jerusalem:20260812T093000\nRRULE:FREQ=WEEKLY;BYDAY=WE\nUID:x\nSUMMARY:local";
        assert_eq!(one(e, DAY)[0].start, Some((8, 30)));
    }

    const JUDO: &str = "DTSTART;TZID=Asia/Jerusalem:20260902T130000\nDTEND;TZID=Asia/Jerusalem:20260902T140000\nRRULE:FREQ=WEEKLY;WKST=MO;UNTIL=20270630T205959Z\nUID:judo\nSUMMARY:13:00Дзюдо";

    #[test]
    fn excluded_dates_are_skipped() {
        let local = format!("{JUDO}\nEXDATE;TZID=Asia/Jerusalem:20261118T130000,20261125T130000");
        assert_eq!(one(&local, DAY), vec![]);
        assert_eq!(one(&local, date(2026, 11, 18)), vec![]);
        assert_eq!(one(&local, date(2026, 12, 2)).len(), 1, "other weeks still happen");
        let utc = format!("{JUDO}\nEXDATE:20261125T110000Z");
        assert_eq!(one(&utc, DAY), vec![], "same instant written in UTC");
        let all_day = "DTSTART;VALUE=DATE:20261104\nDTEND;VALUE=DATE:20261105\nRRULE:FREQ=WEEKLY\nEXDATE;VALUE=DATE:20261125\nUID:w\nSUMMARY:x";
        assert_eq!(one(all_day, DAY), vec![]);
        assert_eq!(one(all_day, date(2026, 11, 18)).len(), 1);
    }

    #[test]
    fn edited_occurrence_replaces_the_series_on_that_day() {
        let moved = "RECURRENCE-ID;TZID=Asia/Jerusalem:20261125T130000\nDTSTART;TZID=Asia/Jerusalem:20261125T150000\nDTEND;TZID=Asia/Jerusalem:20261125T160000\nUID:judo\nSUMMARY:Дзюдо (later)";
        let want = vec![ev(Some((15, 0)), Some((16, 0)), "Дзюдо (later)")];
        assert_eq!(events_on(&ics(&[JUDO, moved]), DAY), want);
        assert_eq!(events_on(&ics(&[moved, JUDO]), DAY), want, "order in the file must not matter");
        assert_eq!(events_on(&ics(&[JUDO, moved]), date(2026, 12, 2)).len(), 1, "other weeks unchanged");
    }

    #[test]
    fn occurrence_moved_to_another_day() {
        let moved = "RECURRENCE-ID;TZID=Asia/Jerusalem:20261125T130000\nDTSTART;TZID=Asia/Jerusalem:20261126T130000\nDTEND;TZID=Asia/Jerusalem:20261126T140000\nUID:judo\nSUMMARY:13:00Дзюдо";
        assert_eq!(events_on(&ics(&[JUDO, moved]), DAY), vec![]);
        assert_eq!(events_on(&ics(&[JUDO, moved]), date(2026, 11, 26)).len(), 1);
    }

    #[test]
    fn cancelled_events_are_hidden() {
        let single = "DTSTART:20261125T080000Z\nDTEND:20261125T090000Z\nUID:s\nSUMMARY:gone\nSTATUS:CANCELLED";
        assert_eq!(one(single, DAY), vec![]);
        let cancelled_occurrence = "RECURRENCE-ID;TZID=Asia/Jerusalem:20261125T130000\nDTSTART;TZID=Asia/Jerusalem:20261125T130000\nDTEND;TZID=Asia/Jerusalem:20261125T140000\nUID:judo\nSUMMARY:13:00Дзюдо\nSTATUS:CANCELLED";
        assert_eq!(events_on(&ics(&[JUDO, cancelled_occurrence]), DAY), vec![]);
    }

}
