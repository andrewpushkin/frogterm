//! `config.toml`: album link and city for the weather (shared with the firmware later).

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    /// Google Photos share link, e.g. `https://photos.app.goo.gl/XXXX`.
    pub album_url: String,
    /// City for the weather, looked up by name (e.g. "Tel Aviv").
    pub city: String,
    /// Label shown in the weather box; defaults to `city`.
    #[serde(default)]
    pub place_name: Option<String>,
    /// Exact point for the weather (both or neither); overrides the city lookup.
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    /// iCal links ("secret address in iCal format" or a public one), merged.
    #[serde(default)]
    pub calendars: Vec<String>,
    /// Boxes over the photo: 100 = solid white, lower = photo shows through
    /// (lightened by this percentage), 0 = fully transparent. Default 50.
    #[serde(default = "half")]
    pub overlay_opacity: u8,
    /// Firmware only: Wi-Fi network.
    #[serde(default)]
    pub wifi_ssid: Option<String>,
    #[serde(default)]
    pub wifi_password: Option<String>,
    /// Firmware only: local hour of the daily update (default 6).
    #[serde(default = "six")]
    pub update_hour: u8,
    /// Firmware only: photos downloaded per Wi-Fi batch (1..=20, default 10).
    #[serde(default = "ten")]
    pub photo_batch: u8,
    /// Firmware only: weather/calendar older than this are fetched again;
    /// younger ones are reused without Wi-Fi (default 480 = 8 h, max 1080).
    #[serde(default = "eight_hours")]
    pub refresh_minutes: u32,
    /// Local time zone as a POSIX TZ string (default: Israel).
    #[serde(default = "israel")]
    pub timezone: String,
}

fn israel() -> String {
    frogterm_core::tz::ISRAEL.to_string()
}

fn ten() -> u8 {
    10
}

fn eight_hours() -> u32 {
    480
}

fn six() -> u8 {
    6
}

fn half() -> u8 {
    50
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text)?;
        if !c.album_url.starts_with("https://") {
            bail!("album_url must be an https:// link, got {:?}", c.album_url);
        }
        if c.city.trim().is_empty() {
            bail!("city is empty");
        }
        if let Some(bad) = c.calendars.iter().find(|u| !u.starts_with("https://")) {
            bail!("calendar links must be https://, got {bad:?}");
        }
        if frogterm_core::tz::Zone::parse(&c.timezone).is_none() {
            bail!(
                "timezone must be a POSIX TZ string like \"IST-2IDT,M3.4.4/26,M10.5.0\" (Israel), got {:?}",
                c.timezone
            );
        }
        if !(1..=20).contains(&c.photo_batch) {
            bail!("photo_batch must be 1..=20 (screens that fit in flash), got {}", c.photo_batch);
        }
        if !(1..=1080).contains(&c.refresh_minutes) {
            bail!("refresh_minutes must be 1..=1080 (at most 18 h between weather fetches)");
        }
        if c.update_hour > 23 {
            bail!("update_hour must be 0..=23, got {}", c.update_hour);
        }
        if c.overlay_opacity > 100 {
            bail!("overlay_opacity is a percentage 0..=100, got {}", c.overlay_opacity);
        }
        if c.latitude.is_some() != c.longitude.is_some() {
            bail!("set both latitude and longitude, or neither");
        }
        Ok(c)
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!("cannot read {} (copy config.example.toml to config.toml and edit it)", path.display())
        })?;
        Config::parse(&text).with_context(|| format!("bad config in {}", path.display()))
    }

    /// Name shown in the weather box.
    pub fn label(&self) -> &str {
        self.place_name.as_deref().unwrap_or(&self.city)
    }

    /// The configured time zone (validated in `parse`).
    pub fn zone(&self) -> frogterm_core::tz::Zone {
        frogterm_core::tz::Zone::parse(&self.timezone).expect("validated when parsed")
    }

    /// Fixed coordinates, if the config pins them.
    pub fn coordinates(&self) -> Option<(f64, f64)> {
        self.latitude.zip(self.longitude)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_has_link_and_city() {
        let c = Config::parse("album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n").unwrap();
        assert_eq!(c.album_url, "https://photos.app.goo.gl/abc");
        assert_eq!(c.label(), "Tel Aviv");
        assert_eq!(c.coordinates(), None);
    }

    #[test]
    fn optional_place_name_and_coordinates() {
        let c = Config::parse(
            "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n\
             place_name = \"Tel Aviv\"\nlatitude = 32.0853\nlongitude = 34.7818\n",
        )
        .unwrap();
        assert_eq!(c.label(), "Tel Aviv");
        assert_eq!(c.coordinates(), Some((32.0853, 34.7818)));
    }

    #[test]
    fn calendars_are_optional_https_links() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        assert!(Config::parse(base).unwrap().calendars.is_empty());
        let c = Config::parse(&format!(
            "{base}calendars = [\"https://calendar.google.com/calendar/ical/a/private-x/basic.ics\", \"https://calendar.google.com/calendar/ical/b/public/basic.ics\"]\n"
        ))
        .unwrap();
        assert_eq!(c.calendars.len(), 2);
        assert!(Config::parse(&format!("{base}calendars = [\"webcal://x\"]\n")).is_err());
    }

    #[test]
    fn overlay_opacity_defaults_to_50_and_is_a_percentage() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        assert_eq!(Config::parse(base).unwrap().overlay_opacity, 50);
        assert_eq!(Config::parse(&format!("{base}overlay_opacity = 60\n")).unwrap().overlay_opacity, 60);
        assert!(Config::parse(&format!("{base}overlay_opacity = 101\n")).is_err());
    }

    #[test]
    fn firmware_settings_parse() {
        let c = Config::parse(
            "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\nwifi_ssid = \"home\"\nwifi_password = \"pw\"\nupdate_hour = 7\n",
        )
        .unwrap();
        assert_eq!(c.wifi_ssid.as_deref(), Some("home"));
        assert_eq!(c.wifi_password.as_deref(), Some("pw"));
        assert_eq!(c.update_hour, 7);
    }

    #[test]
    fn update_hour_defaults_to_6_and_is_validated() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        assert_eq!(Config::parse(base).unwrap().update_hour, 6);
        assert!(Config::parse(&format!("{base}update_hour = 24\n")).is_err());
    }

    #[test]
    fn photo_batch_and_refresh_minutes() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        let c = Config::parse(base).unwrap();
        assert_eq!((c.photo_batch, c.refresh_minutes), (10, 480), "owner: every 8 h is fine");
        let c = Config::parse(&format!("{base}photo_batch = 5\nrefresh_minutes = 30\n")).unwrap();
        assert_eq!((c.photo_batch, c.refresh_minutes), (5, 30));
        assert!(Config::parse(&format!("{base}photo_batch = 0\n")).is_err());
        assert!(Config::parse(&format!("{base}photo_batch = 21\n")).is_err(), "only 20 screens fit in flash");
        assert!(Config::parse(&format!("{base}refresh_minutes = 0\n")).is_err());
        assert!(Config::parse(&format!("{base}refresh_minutes = 1081\n")).is_err(), "at most 18 h between weather fetches");
    }

    #[test]
    fn the_demo_config_is_valid() {
        let c = Config::parse(include_str!("../../config.example.toml")).expect("config.example.toml must parse");
        assert_eq!(c.calendars.len(), 1, "the public holidays calendar");
        assert!(c.wifi_ssid.is_some());
    }

    #[test]
    fn timezone_defaults_to_israel_and_must_be_posix() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        assert_eq!(Config::parse(base).unwrap().timezone, "IST-2IDT,M3.4.4/26,M10.5.0");
        let c = Config::parse(&format!("{base}timezone = \"CET-1CEST,M3.5.0,M10.5.0/3\"\n")).unwrap();
        assert_eq!(c.zone().std_offset, 3600);
        let e = Config::parse(&format!("{base}timezone = \"Asia/Jerusalem\"\n")).unwrap_err();
        assert!(format!("{e:#}").contains("POSIX"), "{e:#}");
    }

    #[test]
    fn firmware_only_keys_are_ignored() {
        let c = Config::parse(
            "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\nwifi_ssid = \"home\"\nupdate_hour = 6\n",
        );
        assert!(c.is_ok());
    }

    #[test]
    fn latitude_without_longitude_is_rejected() {
        let e = Config::parse("album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\nlatitude = 32.7\n");
        assert!(e.is_err());
    }

    #[test]
    fn missing_or_bad_values_are_rejected() {
        assert!(Config::parse("city = \"Tel Aviv\"\n").is_err());
        assert!(Config::parse("album_url = \"https://photos.app.goo.gl/abc\"\n").is_err());
        assert!(Config::parse("album_url = \"photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n").is_err());
        assert!(Config::parse("album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"  \"\n").is_err());
    }

    #[test]
    fn missing_file_explains_what_to_do() {
        let e = Config::load(Path::new("/nonexistent/config.toml")).unwrap_err();
        assert!(format!("{e:#}").contains("config.example.toml"), "{e:#}");
    }
}
