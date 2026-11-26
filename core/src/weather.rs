//! Hourly forecast from Open-Meteo (free, no API key) and city lookup.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// Number of hourly slots shown on screen: now, +2 h, +4 h … +12 h.
pub const SLOTS: usize = 7;
/// Hours between two shown slots.
pub const STEP_HOURS: usize = 2;
/// Hours fetched (today and tomorrow), so every later offline update can show
/// its own next 12 hours from the stored forecast.
pub const HOURS: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Drizzle,
    Rain,
    Showers,
    Thunder,
    Snow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HourSlot {
    /// Local hour 0–23.
    pub hour: u8,
    pub temp_c: f32,
    /// Precipitation probability 0–100.
    pub precip_pct: u8,
    /// UV index (0 at night).
    pub uv: f32,
    /// Daylight at this hour (else the icon shows the moon instead of the sun).
    pub is_day: bool,
    /// Moon phase at this hour (0 new, 0.5 full).
    pub moon: f32,
    pub icon: Icon,
}

/// Sunrise and sunset of one day (local times).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SunDay {
    pub date: crate::date::Date,
    pub rise: (u8, u8),
    pub set: (u8, u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    /// Local time of the forecast's "current" step (15-minute resolution), `HH:MM`.
    pub now: String,
    /// Offset of the place's local time from UTC (DST included).
    pub utc_offset_secs: i32,
    /// Unix time of the first slot's hour.
    pub start_unix: i64,
    /// Consecutive hours from `start_unix`.
    pub slots: Vec<HourSlot>,
    /// Sunrise/sunset of the next days.
    pub sun: Vec<SunDay>,
}

impl Forecast {
    pub fn sun_on(&self, date: crate::date::Date) -> Option<&SunDay> {
        self.sun.iter().find(|s| s.date == date)
    }

    /// The hour that contains `now` and every `STEP_HOURS`-th hour after it,
    /// up to `SLOTS` of them (fewer if the stored forecast ends sooner), or
    /// `None` if the stored forecast no longer reaches `now`.
    pub fn window(&self, now: i64) -> Option<impl Iterator<Item = &HourSlot>> {
        let first = usize::try_from((now - self.start_unix).div_euclid(3600)).unwrap_or(0);
        self.slots.get(first..).filter(|s| !s.is_empty()).map(|s| s.iter().step_by(STEP_HOURS).take(SLOTS))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// Forecast request: local times (`timezone=auto`), starting at the current hour.
pub fn forecast_url(latitude: f64, longitude: f64) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude:.4}&longitude={longitude:.4}\
         &hourly=temperature_2m,precipitation_probability,weather_code,uv_index,is_day\
         &daily=sunrise,sunset\
         &current=temperature_2m&timezone=auto&forecast_hours={HOURS}"
    )
}

pub fn parse_forecast(json: &str) -> Result<Forecast> {
    let v: Value = serde_json::from_str(json).context("forecast is not JSON")?;
    if let Some(reason) = v["reason"].as_str() {
        bail!("Open-Meteo error: {reason}");
    }
    let now = v["current"]["time"].as_str().and_then(clock).ok_or_else(|| anyhow!("forecast has no current time"))?.to_string();
    let h = &v["hourly"];
    let times = h["time"].as_array().ok_or_else(|| anyhow!("forecast has no hourly times"))?;
    let mut slots = Vec::with_capacity(HOURS);
    for (i, t) in times.iter().take(HOURS).enumerate() {
        let hour = t.as_str().and_then(clock).and_then(|c| c[..2].parse::<u8>().ok()).ok_or_else(|| anyhow!("bad hourly time {t}"))?;
        let temp_c = h["temperature_2m"][i].as_f64().ok_or_else(|| anyhow!("no temperature for {hour}:00"))? as f32;
        let precip_pct = h["precipitation_probability"][i].as_u64().unwrap_or(0).min(100) as u8;
        let code = h["weather_code"][i].as_u64().and_then(|c| u16::try_from(c).ok()).unwrap_or(u16::MAX);
        let uv = h["uv_index"][i].as_f64().unwrap_or(0.0) as f32;
        let is_day = h["is_day"][i].as_u64() != Some(0);
        slots.push(HourSlot { hour, temp_c, precip_pct, uv, is_day, moon: 0.0, icon: icon_for(code) });
    }
    if slots.is_empty() {
        bail!("forecast has no hours");
    }
    let utc_offset_secs = v["utc_offset_seconds"].as_i64().and_then(|o| i32::try_from(o).ok()).unwrap_or(0);
    let first = times[0].as_str().ok_or_else(|| anyhow!("bad hourly time"))?;
    let start_unix = local_iso_to_unix(first, utc_offset_secs).ok_or_else(|| anyhow!("bad hourly time {first}"))?;
    for (i, s) in slots.iter_mut().enumerate() {
        s.moon = crate::moon::phase(start_unix + i as i64 * 3600);
    }
    let d = &v["daily"];
    let hm = |iso: &Value| -> Option<(u8, u8)> {
        let c = clock(iso.as_str()?)?;
        Some((c[..2].parse().ok()?, c[3..5].parse().ok()?))
    };
    let sun = (0..d["time"].as_array().map_or(0, Vec::len))
        .filter_map(|k| {
            let t = d["time"][k].as_str()?;
            let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<u32>().ok();
            let date = crate::date::Date { year: num(0..4)? as i32, month: num(5..7)? as u8, day: num(8..10)? as u8 };
            Some(SunDay { date, rise: hm(&d["sunrise"][k])?, set: hm(&d["sunset"][k])? })
        })
        .collect();
    Ok(Forecast { now, utc_offset_secs, start_unix, slots, sun })
}

/// `"2026-11-25T13:00"` (local) → Unix time.
fn local_iso_to_unix(iso: &str, utc_offset_secs: i32) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let date = crate::date::Date { year: num(0..4)? as i32, month: num(5..7)? as u8, day: num(8..10)? as u8 };
    let secs = num(11..13)? * 3600 + num(14..16)? * 60;
    Some(date.days_since_epoch() * 86_400 + secs - i64::from(utc_offset_secs))
}

/// `"2026-11-25T13:00"` → `"13:00"`.
fn clock(iso: &str) -> Option<&str> {
    let t = iso.split_once('T')?.1;
    (t.len() >= 5 && t.as_bytes()[2] == b':').then(|| &t[..5])
}

/// WMO weather interpretation code → icon.
pub fn icon_for(code: u16) -> Icon {
    match code {
        0 | 1 => Icon::Clear,
        2 => Icon::PartlyCloudy,
        45 | 48 => Icon::Fog,
        51..=57 => Icon::Drizzle,
        61..=67 => Icon::Rain,
        71..=77 | 85 | 86 => Icon::Snow,
        80..=82 => Icon::Showers,
        95..=99 => Icon::Thunder,
        _ => Icon::Cloudy,
    }
}

/// City lookup request (Open-Meteo geocoding).
pub fn geocode_url(city: &str) -> String {
    format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json", crate::url::percent_encode(city))
}

/// First match of a geocoding response.
pub fn parse_geocode(json: &str) -> Result<Place> {
    let v: Value = serde_json::from_str(json).context("geocoding answer is not JSON")?;
    let r = &v["results"][0];
    match (r["name"].as_str(), r["latitude"].as_f64(), r["longitude"].as_f64()) {
        (Some(name), Some(latitude), Some(longitude)) => Ok(Place { name: name.to_string(), latitude, longitude }),
        _ => bail!("city not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real Open-Meteo response shape (2026-11-25, Tel Aviv), trimmed.
    const FORECAST: &str = r#"{"latitude":32.0853,"longitude":34.7818,"utc_offset_seconds":7200,
        "timezone":"Asia/Jerusalem","current_units":{"time":"iso8601","interval":"seconds","temperature_2m":"°C"},
        "current":{"time":"2026-11-25T13:00","interval":900,"temperature_2m":21.5},
        "hourly":{"time":["2026-11-25T13:00","2026-11-25T14:00","2026-11-25T15:00","2026-11-25T16:00",
                          "2026-11-25T17:00","2026-11-25T18:00","2026-11-25T19:00"],
                  "temperature_2m":[21.5,21.0,20.6,19.6,18.4,17.2,16.0],
                  "precipitation_probability":[0,0,10,35,80,null,0],
                  "weather_code":[3,2,2,61,95,0,0],
                  "uv_index":[3.4,2.9,1.65,0.6,0.1,0.0,0.0],
                  "is_day":[1,1,1,1,0,0,0]},
        "daily":{"time":["2026-11-25","2026-11-26"],
                 "sunrise":["2026-11-25T06:21","2026-11-26T06:22"],
                 "sunset":["2026-11-25T16:38","2026-11-26T16:38"]}}"#;

    #[test]
    fn forecast_url_asks_for_two_days_of_local_hours() {
        let u = forecast_url(32.0853, 34.7818);
        assert!(u.starts_with("https://api.open-meteo.com/v1/forecast?"), "{u}");
        for part in [
            "latitude=32.0853",
            "longitude=34.7818",
            "hourly=temperature_2m,precipitation_probability,weather_code,uv_index,is_day",
            "daily=sunrise,sunset",
            "current=temperature_2m",
            "timezone=auto",
            "forecast_hours=48",
        ] {
            assert!(u.contains(part), "{u} lacks {part}");
        }
    }

    #[test]
    fn parse_forecast_keeps_every_hour_from_the_current_one() {
        let f = parse_forecast(FORECAST).unwrap();
        assert_eq!(f.now, "13:00");
        assert_eq!(f.utc_offset_secs, 7200);
        assert_eq!(f.slots.len(), 7, "all hours of the answer (up to HOURS)");
        // 2026-11-25 13:00 local (UTC+2) = 11:00 UTC
        assert_eq!(f.start_unix, crate::date::Date { year: 2026, month: 11, day: 25 }.days_since_epoch() * 86_400 + 11 * 3600);
        assert_eq!(f.slots[0].hour, 13);
        assert_eq!((f.slots[0].temp_c, f.slots[0].precip_pct, f.slots[0].uv), (21.5, 0, 3.4));
        assert_eq!(f.slots[0].icon, Icon::Cloudy);
        assert_eq!(f.slots[3].icon, Icon::Rain);
        assert_eq!(f.slots[3].precip_pct, 35);
        assert_eq!(f.slots[4].icon, Icon::Thunder);
        assert_eq!(f.slots[5].hour, 18);
    }

    /// `n` consecutive hours from 13:00 (UTC+2), as stored after a fetch.
    fn hours(n: u8) -> Forecast {
        let mut f = parse_forecast(FORECAST).unwrap();
        f.slots = (0..n)
            .map(|i| HourSlot {
                hour: (13 + i) % 24,
                temp_c: f32::from(i),
                precip_pct: 0,
                uv: 0.0,
                is_day: true,
                moon: 0.0,
                icon: Icon::Clear,
            })
            .collect();
        f
    }

    #[test]
    fn window_is_now_and_every_second_hour_up_to_12_h() {
        let f = hours(HOURS as u8);
        let shown: Vec<u8> = f.window(f.start_unix + 30 * 60).unwrap().map(|s| s.hour).collect();
        assert_eq!(shown, [13, 15, 17, 19, 21, 23, 1], "now, +2 h … +12 h");
        let mut w = f.window(f.start_unix - 600).unwrap();
        assert_eq!(w.next().unwrap().hour, 13, "slightly before the first hour still starts there");
    }

    #[test]
    fn later_updates_take_the_current_hour_from_the_cache() {
        let f = hours(HOURS as u8);
        let shown: Vec<u8> = f.window(f.start_unix + 7 * 3600 + 5).unwrap().map(|s| s.hour).collect();
        assert_eq!(shown, [20, 22, 0, 2, 4, 6, 8]);
    }

    #[test]
    fn a_short_cache_shows_what_it_still_has() {
        let f = hours(7); // 13..19
        let shown: Vec<u8> = f.window(f.start_unix).unwrap().map(|s| s.hour).collect();
        assert_eq!(shown, [13, 15, 17, 19]);
        assert_eq!(f.window(f.start_unix + 6 * 3600).unwrap().count(), 1, "only 19:00 left");
        assert!(f.window(f.start_unix + 7 * 3600).is_none(), "nothing left for now");
    }

    #[test]
    fn a_day_old_fetch_still_covers_the_next_12_hours() {
        let f = hours(HOURS as u8);
        assert_eq!(f.window(f.start_unix + 24 * 3600).unwrap().count(), SLOTS);
    }

    #[test]
    fn night_hours_and_moon_phase() {
        let f = parse_forecast(FORECAST).unwrap();
        assert!(f.slots[3].is_day && !f.slots[4].is_day, "16:00 day, 17:00 night");
        // 2026-11-25 is the day after a full moon
        assert!((f.slots[4].moon - 0.5).abs() < 0.05, "{}", f.slots[4].moon);
    }

    #[test]
    fn sunrise_and_sunset_per_day() {
        let f = parse_forecast(FORECAST).unwrap();
        let d = crate::date::Date { year: 2026, month: 11, day: 26 };
        assert_eq!(f.sun_on(d), Some(&SunDay { date: d, rise: (6, 22), set: (16, 38) }));
        assert_eq!(f.sun_on(crate::date::Date { year: 2026, month: 12, day: 1 }), None);
    }

    #[test]
    fn missing_precipitation_counts_as_zero() {
        assert_eq!(parse_forecast(FORECAST).unwrap().slots[5].precip_pct, 0);
    }

    #[test]
    fn forecast_without_hours_is_an_error() {
        assert!(parse_forecast(r#"{"current":{"time":"2026-11-25T13:00"},"hourly":{"time":[]}}"#).is_err());
        assert!(parse_forecast(r#"{"error":true,"reason":"bad"}"#).is_err());
        assert!(parse_forecast("not json").is_err());
    }

    #[test]
    fn wmo_codes_map_to_icons() {
        let cases = [
            (0, Icon::Clear),
            (1, Icon::Clear),
            (2, Icon::PartlyCloudy),
            (3, Icon::Cloudy),
            (45, Icon::Fog),
            (48, Icon::Fog),
            (51, Icon::Drizzle),
            (57, Icon::Drizzle),
            (61, Icon::Rain),
            (67, Icon::Rain),
            (71, Icon::Snow),
            (77, Icon::Snow),
            (80, Icon::Showers),
            (82, Icon::Showers),
            (85, Icon::Snow),
            (95, Icon::Thunder),
            (99, Icon::Thunder),
            (500, Icon::Cloudy),
        ];
        for (code, icon) in cases {
            assert_eq!(icon_for(code), icon, "code {code}");
        }
    }

    #[test]
    fn geocode_url_encodes_city() {
        let u = geocode_url("Tel Aviv-Yafo");
        assert!(u.starts_with("https://geocoding-api.open-meteo.com/v1/search?"), "{u}");
        assert!(u.contains("name=Tel%20Aviv-Yafo"), "{u}");
        assert!(u.contains("count=1"), "{u}");
        assert!(geocode_url("תל אביב").contains("name=%D7%AA%D7%9C%20%D7%90%D7%91%D7%99%D7%91"));
    }

    #[test]
    fn parse_geocode_takes_first_result() {
        let json = r#"{"results":[{"id":294801,"name":"Tel Aviv","latitude":32.08088,"longitude":34.78057,
            "country":"Israel","timezone":"Asia/Jerusalem"}],"generationtime_ms":0.5}"#;
        assert_eq!(parse_geocode(json).unwrap(), Place { name: "Tel Aviv".into(), latitude: 32.08088, longitude: 34.78057 });
    }

    #[test]
    fn unknown_city_is_an_error() {
        assert!(parse_geocode(r#"{"generationtime_ms":0.3}"#).is_err());
    }
}
