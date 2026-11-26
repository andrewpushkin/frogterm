//! Settings from `config.toml`, baked in at build time by `build.rs`.

pub struct Settings {
    pub album_url: &'static str,
    pub city: &'static str,
    /// Label in the events box header.
    pub place: &'static str,
    pub coords: Option<(f64, f64)>,
    pub calendars: Vec<&'static str>,
    pub opacity: u8,
    pub wifi_ssid: &'static str,
    pub wifi_password: &'static str,
    /// Local `(hour, minute)` of the daily screen updates.
    pub update_times: Vec<(u8, u8)>,
    pub photo_batch: u8,
    pub refresh_minutes: u32,
    /// POSIX TZ string (validated at build time).
    pub timezone: &'static str,
}

pub fn settings() -> Settings {
    let num = |s: &str| s.parse::<f64>().ok();
    Settings {
        album_url: env!("FT_ALBUM_URL"),
        city: env!("FT_CITY"),
        place: env!("FT_PLACE"),
        coords: num(env!("FT_LAT")).zip(num(env!("FT_LON"))),
        calendars: env!("FT_CALENDARS").split("\\n").filter(|s| !s.is_empty()).collect(),
        opacity: env!("FT_OPACITY").parse().unwrap_or(50),
        wifi_ssid: env!("FT_WIFI_SSID"),
        wifi_password: env!("FT_WIFI_PASSWORD"),
        update_times: frogterm_core::schedule::parse_times(env!("FT_UPDATE_TIMES"))
            .unwrap_or_else(|| frogterm_core::schedule::DEFAULT_TIMES.to_vec()),
        photo_batch: env!("FT_PHOTO_BATCH").parse().unwrap_or(10),
        refresh_minutes: env!("FT_REFRESH_MINUTES").parse().unwrap_or(480),
        timezone: env!("FT_TIMEZONE"),
    }
}

/// Host part of a URL, for logs (links contain secrets).
pub fn host(url: &str) -> &str {
    url.split('/').nth(2).unwrap_or("?")
}
