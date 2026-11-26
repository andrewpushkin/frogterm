//! Weather and today's events as last fetched, kept between wakes so an
//! update within the refresh time needs no Wi-Fi.

use crate::calendar::DayEvent;
use crate::date::Date;
use crate::weather::{Forecast, HourSlot, Icon};

/// Bump when the format gains fields, so data from older firmware is fetched again.
const HEADER: &str = "frogterm-info v3";
const ICONS: [Icon; 9] = [
    Icon::Clear,
    Icon::PartlyCloudy,
    Icon::Cloudy,
    Icon::Fog,
    Icon::Drizzle,
    Icon::Rain,
    Icon::Showers,
    Icon::Thunder,
    Icon::Snow,
];

fn hm(t: Option<(u8, u8)>) -> String {
    t.map_or("-".into(), |(h, m)| format!("{h:02}:{m:02}"))
}

fn parse_hm(s: &str) -> Option<Option<(u8, u8)>> {
    if s == "-" {
        return Some(None);
    }
    let (h, m) = s.split_once(':')?;
    Some(Some((h.parse().ok()?, m.parse().ok()?)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// Unix time of the fetch.
    pub fetched_at: i64,
    /// Local date the events belong to.
    pub date: Date,
    pub forecast: Option<Forecast>,
    /// All events of `date` (filter with `calendar::remaining` when showing).
    pub events: Option<Vec<DayEvent>>,
    /// All events of the day after `date`.
    pub tomorrow: Option<Vec<DayEvent>>,
}

impl Info {
    /// Stored (today's, tomorrow's) events as seen on local date `today`: after
    /// midnight the stored "tomorrow" are today's events; older ones are gone.
    pub fn events_for(&self, today: Date) -> (Option<Vec<DayEvent>>, Option<Vec<DayEvent>>) {
        let days = today.days_since_epoch() - self.date.days_since_epoch();
        match days {
            0 => (self.events.clone(), self.tomorrow.clone()),
            1 => (self.tomorrow.clone(), None),
            _ => (None, None),
        }
    }

    pub fn to_text(&self) -> String {
        let d = self.date;
        let mut s = format!("{HEADER}\nfetched {}\ndate {:04}-{:02}-{:02}\n", self.fetched_at, d.year, d.month, d.day);
        match &self.forecast {
            None => s.push_str("forecast none\n"),
            Some(f) => {
                s.push_str(&format!("forecast {} {} {}\n", f.utc_offset_secs, f.start_unix, f.now));
                for h in &f.slots {
                    let icon = ICONS.iter().position(|&i| i == h.icon).unwrap_or(2);
                    s.push_str(&format!(
                        "h {} {} {} {} {} {} {}\n",
                        h.hour,
                        h.temp_c,
                        h.precip_pct,
                        icon,
                        h.uv,
                        u8::from(h.is_day),
                        h.moon
                    ));
                }
                for sd in &f.sun {
                    let d = sd.date;
                    s.push_str(&format!(
                        "sun {:04}-{:02}-{:02} {} {}\n",
                        d.year,
                        d.month,
                        d.day,
                        hm(Some(sd.rise)),
                        hm(Some(sd.set))
                    ));
                }
            }
        }
        for (tag, line, list) in [("events", "e", &self.events), ("tomorrow", "t", &self.tomorrow)] {
            match list {
                None => s.push_str(&format!("{tag} none\n")),
                Some(ev) => {
                    s.push_str(&format!("{tag} {}\n", ev.len()));
                    for e in ev {
                        s.push_str(&format!("{line} {} {} {}\n", hm(e.start), hm(e.end), e.title.replace('\n', " ")));
                    }
                }
            }
        }
        s
    }

    pub fn from_text(text: &str) -> Option<Info> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let fetched_at = lines.next()?.strip_prefix("fetched ")?.parse().ok()?;
        let ds = lines.next()?.strip_prefix("date ")?;
        let date = Date { year: ds.get(0..4)?.parse().ok()?, month: ds.get(5..7)?.parse().ok()?, day: ds.get(8..10)?.parse().ok()? };
        let mut forecast = None;
        let mut events = None;
        let mut tomorrow = None;
        for line in lines {
            let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
            match tag {
                "forecast" if rest != "none" => {
                    let mut f = rest.splitn(3, ' ');
                    forecast = Some(Forecast {
                        utc_offset_secs: f.next()?.parse().ok()?,
                        start_unix: f.next()?.parse().ok()?,
                        now: f.next()?.to_string(),
                        slots: Vec::new(),
                        sun: Vec::new(),
                    });
                }
                "h" => {
                    let mut f = rest.split(' ');
                    let slot = HourSlot {
                        hour: f.next()?.parse().ok()?,
                        temp_c: f.next()?.parse().ok()?,
                        precip_pct: f.next()?.parse().ok()?,
                        icon: *ICONS.get(f.next()?.parse::<usize>().ok()?)?,
                        uv: f.next()?.parse().ok()?,
                        is_day: f.next()? == "1",
                        moon: f.next()?.parse().ok()?,
                    };
                    forecast.as_mut()?.slots.push(slot);
                }
                "sun" => {
                    let mut f = rest.split(' ');
                    let ds = f.next()?;
                    let date = Date { year: ds.get(0..4)?.parse().ok()?, month: ds.get(5..7)?.parse().ok()?, day: ds.get(8..10)?.parse().ok()? };
                    let rise = parse_hm(f.next()?)??;
                    let set = parse_hm(f.next()?)??;
                    forecast.as_mut()?.sun.push(crate::weather::SunDay { date, rise, set });
                }
                "events" if rest != "none" => events = Some(Vec::new()),
                "tomorrow" if rest != "none" => tomorrow = Some(Vec::new()),
                "e" | "t" => {
                    let mut f = rest.splitn(3, ' ');
                    let start = parse_hm(f.next()?)?;
                    let end = parse_hm(f.next()?)?;
                    let title = f.next().unwrap_or("").to_string();
                    let list = if tag == "e" { &mut events } else { &mut tomorrow };
                    list.as_mut()?.push(DayEvent { start, end, title });
                }
                _ => {}
            }
        }
        Some(Info { fetched_at, date, forecast, events, tomorrow })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info {
        let icons = [Icon::Clear, Icon::PartlyCloudy, Icon::Cloudy, Icon::Fog, Icon::Drizzle, Icon::Rain, Icon::Showers, Icon::Thunder, Icon::Snow];
        Info {
            fetched_at: 1_795_600_000,
            date: Date { year: 2026, month: 11, day: 25 },
            forecast: Some(Forecast {
                now: "13:00".into(),
                utc_offset_secs: 7200,
                start_unix: 1_795_600_800,
                slots: icons
                    .iter()
                    .enumerate()
                    .map(|(i, &icon)| HourSlot {
                        hour: 13 + i as u8,
                        temp_c: 21.5 - i as f32,
                        precip_pct: (i * 10) as u8,
                        uv: i as f32 * 1.5,
                        is_day: i < 4,
                        moon: 0.5 + i as f32 * 0.01,
                        icon,
                    })
                    .collect(),
                sun: vec![crate::weather::SunDay { date: Date { year: 2026, month: 11, day: 25 }, rise: (6, 21), set: (16, 38) }],
            }),
            events: Some(vec![
                DayEvent { start: None, end: None, title: "ДР у друга".into() },
                DayEvent { start: Some((13, 0)), end: Some((14, 0)), title: "Иврит / Язык (שפה) - учитель".into() },
                DayEvent { start: Some((22, 0)), end: None, title: "Night".into() },
            ]),
            tomorrow: Some(vec![DayEvent { start: Some((8, 15)), end: Some((9, 0)), title: "Собрание".into() }]),
        }
    }

    #[test]
    fn round_trips_through_text() {
        assert_eq!(Info::from_text(&info().to_text()), Some(info()));
    }

    #[test]
    fn missing_weather_and_calendar_round_trip() {
        let i = Info { forecast: None, events: None, tomorrow: None, ..info() };
        assert_eq!(Info::from_text(&i.to_text()), Some(i.clone()));
        let empty_day = Info { events: Some(vec![]), tomorrow: Some(vec![]), ..info() };
        assert_eq!(Info::from_text(&empty_day.to_text()), Some(empty_day.clone()));
    }

    #[test]
    fn info_from_older_firmware_is_not_used() {
        // v1 had no tomorrow's events, v2 no day/night or sun times: fetch again
        let v1 = "frogterm-info v1\nfetched 1795600000\ndate 2026-11-25\nforecast none\nevents 0\n";
        assert_eq!(Info::from_text(v1), None);
        assert_eq!(Info::from_text(&v1.replace("v1", "v2")), None);
        assert!(info().to_text().starts_with("frogterm-info v3\n"));
    }

    #[test]
    fn events_move_to_the_right_day() {
        let i = info();
        let day = |d| Date { year: 2026, month: 11, day: d };
        assert_eq!(i.events_for(day(25)), (i.events.clone(), i.tomorrow.clone()));
        assert_eq!(i.events_for(day(26)), (i.tomorrow.clone(), None), "yesterday's tomorrow is today");
        assert_eq!(i.events_for(day(27)), (None, None), "too old");
        assert_eq!(i.events_for(day(24)), (None, None), "clock went back");
    }

    #[test]
    fn garbage_is_not_info() {
        assert_eq!(Info::from_text(""), None);
        assert_eq!(Info::from_text("hello"), None);
        assert_eq!(Info::from_text("frogterm-info v1\nfetched x\n"), None);
    }
}
