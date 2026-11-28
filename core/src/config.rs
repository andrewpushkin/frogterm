//! `config.toml`: every setting of the device, read when it runs (the
//! emulator's file, the device's copy in NVS) and rewritten by the settings
//! page. A small reader for the file's flat shape — `key = "text"`, numbers,
//! lists of text, `#` comments — so the device needs no TOML crate.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// A value as written in the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    /// A number, kept as written.
    Num(String),
    List(Vec<String>),
}

/// The file's `key = value` entries in order.
pub fn parse_flat(text: &str) -> Result<Vec<(String, Value)>> {
    Ok(entries(text)?.into_iter().map(|e| (e.key, e.value)).collect())
}

/// One `key = value` and the lines it takes (0-based, end exclusive).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub key: String,
    pub value: Value,
    pub lines: std::ops::Range<usize>,
}

/// The entries with where they are; an error names the line (1-based).
pub(crate) fn entries(text: &str) -> Result<Vec<Entry>> {
    let mut r = Reader { s: text.as_bytes(), text, pos: 0, line: 0 };
    let mut out: Vec<Entry> = Vec::new();
    loop {
        r.skip_blank(true);
        if r.peek().is_none() {
            return Ok(out);
        }
        let first = r.line;
        let key = r.key()?;
        if out.iter().any(|e| e.key == key) {
            bail!("line {}: {key} is set twice", first + 1);
        }
        r.skip_blank(false);
        if r.peek() != Some(b'=') {
            r.fail("expected `key = value`")?;
        }
        r.pos += 1;
        r.skip_blank(false);
        let value = r.value()?;
        r.skip_blank(false);
        match r.peek() {
            None => {}
            Some(b'\n') => r.bump(),
            Some(_) => r.fail("unexpected text after the value")?,
        }
        let end = if r.peek().is_none() && !text.ends_with('\n') { r.line + 1 } else { r.line };
        out.push(Entry { key, value, lines: first..end.max(first + 1) });
    }
}

struct Reader<'a> {
    s: &'a [u8],
    text: &'a str,
    pos: usize,
    /// 0-based line of `pos`.
    line: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn bump(&mut self) {
        if self.peek() == Some(b'\n') {
            self.line += 1;
        }
        self.pos += 1;
    }

    fn fail<T>(&self, what: &str) -> Result<T> {
        bail!("line {}: {what}", self.line + 1)
    }

    /// Spaces and `#` comments; with `newlines` also line ends.
    fn skip_blank(&mut self, newlines: bool) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\r' => self.bump(),
                b'\n' if newlines => self.bump(),
                b'#' => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    fn key(&mut self) -> Result<String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
            self.bump();
        }
        if start == self.pos {
            return self.fail("expected a setting name (tables like [x] are not supported)");
        }
        Ok(self.text[start..self.pos].to_string())
    }

    fn value(&mut self) -> Result<Value> {
        match self.peek() {
            Some(b'"' | b'\'') => Ok(Value::Str(self.string()?)),
            Some(b'[') => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_blank(true);
                    match self.peek() {
                        Some(b']') => {
                            self.bump();
                            return Ok(Value::List(items));
                        }
                        Some(b'"' | b'\'') => items.push(self.string()?),
                        _ => return self.fail("expected a \"text\" in the list, or ]"),
                    }
                    self.skip_blank(true);
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b']') => {}
                        _ => return self.fail("expected , or ] in the list"),
                    }
                }
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' => {
                let start = self.pos;
                while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || b"+-._".contains(&c)) {
                    self.bump();
                }
                let n = self.text[start..self.pos].replace('_', "");
                if n.parse::<f64>().is_err() {
                    return self.fail("not a number");
                }
                Ok(Value::Num(n))
            }
            _ => self.fail("expected a value: \"text\", a number or a [list]"),
        }
    }

    /// A `"basic"` (with escapes) or `'literal'` string on one line.
    fn string(&mut self) -> Result<String> {
        let quote = self.peek().unwrap_or(b'"');
        self.bump();
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return self.fail("the text is not closed") };
            if c == b'\n' {
                return self.fail("the text is not closed on its line");
            }
            if c == quote {
                self.bump();
                return Ok(out);
            }
            if c == b'\\' && quote == b'"' {
                self.bump();
                let esc = self.peek();
                self.bump();
                match esc {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'n') => out.push('\n'),
                    Some(b't') => out.push('\t'),
                    Some(b'r') => out.push('\r'),
                    Some(n @ (b'u' | b'U')) => {
                        let len = if n == b'u' { 4 } else { 8 };
                        let hex = self.text.get(self.pos..self.pos + len).unwrap_or("");
                        match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                            Some(ch) if hex.len() == len => out.push(ch),
                            _ => return self.fail("bad \\u escape"),
                        }
                        self.pos += len;
                    }
                    _ => return self.fail("unknown \\ escape"),
                }
                continue;
            }
            // a whole UTF-8 character
            let ch = self.text[self.pos..].chars().next().unwrap_or('?');
            out.push(ch);
            self.pos += ch.len_utf8();
        }
    }
}

/// `text` with `key` set to `value` (`None` removes it). The key's lines
/// (all of a multi-line list) are replaced in place; a missing key goes
/// right after its commented-out example (`# key = …`), else at the end.
/// Every other line stays as it was.
pub fn set(text: &str, key: &str, value: Option<&Value>) -> Result<String> {
    let found = entries(text)?.into_iter().find(|e| e.key == key);
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    if let Some(last) = lines.last_mut().filter(|l| !l.ends_with('\n')) {
        last.push('\n');
    }
    let new = value.map(|v| format!("{key} = {}\n", format_value(v)));
    match (found, new) {
        (Some(e), new) => {
            lines.splice(e.lines, new);
        }
        (None, None) => return Ok(text.to_string()),
        (None, Some(new)) => {
            let example = |l: &String| {
                l.trim_start()
                    .strip_prefix('#')
                    .map(str::trim_start)
                    .and_then(|l| l.strip_prefix(key))
                    .is_some_and(|l| l.trim_start().starts_with('='))
            };
            match lines.iter().position(example) {
                Some(i) => lines.insert(i + 1, new),
                None => lines.push(new),
            }
        }
    }
    Ok(lines.concat())
}

/// How `value` is written in the file.
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Str(s) => quote(s),
        Value::Num(n) => n.clone(),
        Value::List(items) => match items.as_slice() {
            [] => "[]".to_string(),
            [one] => format!("[{}]", quote(one)),
            many => format!("[\n{}]", many.iter().map(|s| format!("  {},\n", quote(s))).collect::<String>()),
        },
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Google Photos share link, e.g. `https://photos.app.goo.gl/XXXX`.
    pub album_url: String,
    /// City for the weather, looked up by name (e.g. "Tel Aviv").
    pub city: String,
    /// Label shown in the weather box; defaults to `city`.
    pub place_name: Option<String>,
    /// Exact point for the weather (both or neither); overrides the city lookup.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// iCal links ("secret address in iCal format" or a public one), merged.
    pub calendars: Vec<String>,
    /// Boxes over the photo: 100 = solid white, lower = photo shows through
    /// (lightened by this percentage), 0 = fully transparent. Default 50.
    pub overlay_opacity: u8,
    /// Device only: Wi-Fi network.
    pub wifi_ssid: Option<String>,
    pub wifi_password: Option<String>,
    /// Local times of the daily screen updates, `"HH:MM"`
    /// (default 06:00, 13:30, 18:15).
    pub update_times: Vec<String>,
    /// Device only: photos downloaded per Wi-Fi batch (1..=20, default 10).
    pub photo_batch: u8,
    /// Device only: weather/calendar older than this are fetched again;
    /// younger ones are reused without Wi-Fi (default 480 = 8 h, max 1080).
    pub refresh_minutes: u32,
    /// Local time zone as a POSIX TZ string (default: Israel).
    pub timezone: String,
}

fn default_times() -> Vec<String> {
    crate::schedule::DEFAULT_TIMES.iter().map(|(h, m)| format!("{h:02}:{m:02}")).collect()
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let entries = parse_flat(text)?;
        let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let text = |key: &str| -> Result<Option<String>> {
            match get(key) {
                None => Ok(None),
                Some(Value::Str(s)) => Ok(Some(s.clone())),
                Some(_) => bail!("{key} must be \"text\""),
            }
        };
        let list = |key: &str| -> Result<Option<Vec<String>>> {
            match get(key) {
                None => Ok(None),
                Some(Value::List(l)) => Ok(Some(l.clone())),
                Some(_) => bail!("{key} must be a list like [\"a\", \"b\"]"),
            }
        };
        fn num<T: std::str::FromStr>(key: &str, v: Option<&Value>) -> Result<Option<T>> {
            match v {
                None => Ok(None),
                Some(Value::Num(n)) => match n.parse() {
                    Ok(x) => Ok(Some(x)),
                    Err(_) => bail!("{key}: {n} is out of range"),
                },
                Some(_) => bail!("{key} must be a number (without quotes)"),
            }
        }
        let c = Config {
            album_url: text("album_url")?.context("album_url is missing")?,
            city: text("city")?.context("city is missing")?,
            place_name: text("place_name")?,
            latitude: num("latitude", get("latitude"))?,
            longitude: num("longitude", get("longitude"))?,
            calendars: list("calendars")?.unwrap_or_default(),
            overlay_opacity: num("overlay_opacity", get("overlay_opacity"))?.unwrap_or(50),
            wifi_ssid: text("wifi_ssid")?,
            wifi_password: text("wifi_password")?,
            update_times: list("update_times")?.unwrap_or_else(default_times),
            photo_batch: num("photo_batch", get("photo_batch"))?.unwrap_or(10),
            refresh_minutes: num("refresh_minutes", get("refresh_minutes"))?.unwrap_or(480),
            timezone: text("timezone")?.unwrap_or_else(|| crate::tz::ISRAEL.to_string()),
        };
        c.check()?;
        Ok(c)
    }

    /// `parse`, and on the device (`wifi_needed`) a Wi-Fi network too: it
    /// can do nothing without one.
    pub fn usable(text: &str, wifi_needed: bool) -> Result<Config> {
        let c = Config::parse(text)?;
        if wifi_needed && c.wifi_ssid.as_deref().is_none_or(|s| s.trim().is_empty()) {
            bail!("wifi_ssid is missing (the device needs its Wi-Fi)");
        }
        Ok(c)
    }

    fn check(&self) -> Result<()> {
        let c = self;
        if !c.album_url.starts_with("https://") {
            bail!("album_url must be an https:// link, got {:?}", c.album_url);
        }
        if c.city.trim().is_empty() {
            bail!("city is empty");
        }
        if let Some(bad) = c.calendars.iter().find(|u| !u.starts_with("https://")) {
            bail!("calendar links must be https://, got {bad:?}");
        }
        if crate::tz::Zone::parse(&c.timezone).is_none() {
            bail!("timezone must be a POSIX TZ string like \"IST-2IDT,M3.4.4/26,M10.5.0\" (Israel), got {:?}", c.timezone);
        }
        if !(1..=20).contains(&c.photo_batch) {
            bail!("photo_batch must be 1..=20 (screens that fit in flash), got {}", c.photo_batch);
        }
        if !(1..=1080).contains(&c.refresh_minutes) {
            bail!("refresh_minutes must be 1..=1080 (at most 18 h between weather fetches)");
        }
        if c.update_times.is_empty() {
            bail!("update_times needs at least one \"HH:MM\"");
        }
        if let Some(bad) = c.update_times.iter().find(|t| crate::schedule::parse_time(t).is_none()) {
            bail!("update_times must be local \"HH:MM\" times like \"13:30\", got {bad:?}");
        }
        if c.overlay_opacity > 100 {
            bail!("overlay_opacity is a percentage 0..=100, got {}", c.overlay_opacity);
        }
        if c.latitude.is_some() != c.longitude.is_some() {
            bail!("set both latitude and longitude, or neither");
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {} (copy config.example.toml to config.toml and edit it)", path.display()))?;
        Config::parse(&text).with_context(|| format!("bad config in {}", path.display()))
    }

    /// Name shown in the weather box.
    pub fn label(&self) -> &str {
        self.place_name.as_deref().unwrap_or(&self.city)
    }

    /// The configured time zone (validated in `parse`).
    pub fn zone(&self) -> crate::tz::Zone {
        crate::tz::Zone::parse(&self.timezone).expect("validated when parsed")
    }

    /// The daily update times (validated in `parse`).
    pub fn update_times(&self) -> Vec<(u8, u8)> {
        self.update_times.iter().filter_map(|t| crate::schedule::parse_time(t)).collect()
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
    fn the_device_needs_its_wifi() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        assert!(Config::usable(base, false).is_ok(), "the emulator needs no Wi-Fi settings");
        let e = Config::usable(base, true).unwrap_err();
        assert!(format!("{e:#}").contains("wifi_ssid"), "{e:#}");
        assert!(Config::usable(&format!("{base}wifi_ssid = \"  \"\n"), true).is_err(), "blank is missing");
        assert_eq!(Config::usable(&format!("{base}wifi_ssid = \"home\"\n"), true).unwrap().wifi_ssid.as_deref(), Some("home"));
        assert!(Config::usable("city = \"x\"\n", false).is_err(), "and the other checks still apply");
    }

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
            "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\nwifi_ssid = \"home\"\nwifi_password = \"pw\"\nupdate_times = [\"07:00\", \"19:45\"]\n",
        )
        .unwrap();
        assert_eq!(c.wifi_ssid.as_deref(), Some("home"));
        assert_eq!(c.wifi_password.as_deref(), Some("pw"));
        assert_eq!(c.update_times(), [(7, 0), (19, 45)]);
    }

    #[test]
    fn update_times_default_to_three_a_day_and_are_validated() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        assert_eq!(Config::parse(base).unwrap().update_times(), [(6, 0), (13, 30), (18, 15)]);
        assert!(Config::parse(&format!("{base}update_times = [\"24:00\"]\n")).is_err());
        assert!(Config::parse(&format!("{base}update_times = [\"6\"]\n")).is_err());
        assert!(Config::parse(&format!("{base}update_times = []\n")).is_err());
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
            "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\nwifi_ssid = \"home\"\nwifi_password = \"pw\"\n",
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

    #[test]
    fn multi_line_list_with_comments_parses() {
        let text = "calendars = [\n  # mine\n  \"https://a/x.ics\", # first\n  \"https://b/y.ics\",\n]\nphoto_batch = 5\n";
        let e = parse_flat(text).unwrap();
        assert_eq!(
            e,
            vec![
                ("calendars".to_string(), Value::List(vec!["https://a/x.ics".into(), "https://b/y.ics".into()])),
                ("photo_batch".to_string(), Value::Num("5".into())),
            ]
        );
        assert_eq!(parse_flat("calendars = []\n").unwrap(), vec![("calendars".to_string(), Value::List(vec![]))]);
    }

    #[test]
    fn escapes_and_unicode_parse() {
        let e = parse_flat("place_name = \"תל אביב \\\"x\\\" \\\\ \\u00e9\" # comment\nlatitude = -32.5\n").unwrap();
        assert_eq!(e[0], ("place_name".to_string(), Value::Str("תל אביב \"x\" \\ é".into())));
        assert_eq!(e[1], ("latitude".to_string(), Value::Num("-32.5".into())));
        assert_eq!(parse_flat("city = 'C:\\x'\n").unwrap()[0].1, Value::Str("C:\\x".into()), "literal string");
    }

    #[test]
    fn tables_and_junk_are_refused_with_the_line() {
        let e = parse_flat("city = \"x\"\n\n[wifi]\nssid = \"y\"\n").unwrap_err();
        assert!(format!("{e:#}").contains("line 3"), "{e:#}");
        assert!(parse_flat("city = x\n").is_err(), "bare word");
        assert!(parse_flat("city = \"x\n").is_err(), "unclosed string");
        assert!(parse_flat("city = \"x\" y\n").is_err(), "junk after the value");
        assert!(parse_flat("city = \"x\"\ncity = \"y\"\n").is_err(), "twice");
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_refused() {
        let base = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        let e = Config::parse(&format!("{base}photo_batch = \"5\"\n")).unwrap_err();
        assert!(format!("{e:#}").contains("photo_batch"), "{e:#}");
        assert!(Config::parse(&format!("{base}calendars = \"https://x\"\n")).is_err());
        assert!(Config::parse(&format!("{base}overlay_opacity = 3000\n")).is_err());
    }

    const FILE: &str = "# my frame\nalbum_url = \"https://photos.app.goo.gl/abc\" # the album\ncity = \"Tel Aviv\"\n\n# place_name = \"Home\"\n\ncalendars = [\n  # holidays\n  \"https://a/x.ics\",\n]\nphoto_batch = 5\n";

    #[test]
    fn set_replaces_in_place_and_keeps_comments() {
        let out = set(FILE, "city", Some(&Value::Str("Haifa".into()))).unwrap();
        assert_eq!(out, FILE.replace("city = \"Tel Aviv\"", "city = \"Haifa\""));
        let out = set(FILE, "photo_batch", Some(&Value::Num("7".into()))).unwrap();
        assert!(out.ends_with("photo_batch = 7\n"), "{out}");
        assert!(out.starts_with("# my frame\nalbum_url = \"https://photos.app.goo.gl/abc\" # the album\n"));
    }

    #[test]
    fn set_replaces_a_multi_line_list_as_one() {
        let out = set(FILE, "calendars", Some(&Value::List(vec!["https://b/1.ics".into(), "https://c/2.ics".into()]))).unwrap();
        assert!(!out.contains("holidays") && !out.contains("https://a/x.ics"), "{out}");
        assert!(out.contains("calendars = [\n  \"https://b/1.ics\",\n  \"https://c/2.ics\",\n]\nphoto_batch = 5\n"), "{out}");
        assert_eq!(Config::parse(&out).unwrap().calendars, ["https://b/1.ics", "https://c/2.ics"]);
    }

    #[test]
    fn none_removes_the_key() {
        let out = set(FILE, "calendars", None).unwrap();
        assert!(!out.contains("calendars") && !out.contains("]\n"), "{out}");
        assert!(out.ends_with("# place_name = \"Home\"\n\nphoto_batch = 5\n"), "{out}");
        assert_eq!(set(FILE, "latitude", None).unwrap(), FILE, "not there: nothing to do");
    }

    #[test]
    fn missing_key_goes_after_its_commented_example() {
        let out = set(FILE, "place_name", Some(&Value::Str("Home".into()))).unwrap();
        assert!(out.contains("# place_name = \"Home\"\nplace_name = \"Home\"\n"), "{out}");
        let out = set(FILE, "timezone", Some(&Value::Str("UTC0".into()))).unwrap();
        assert!(out.ends_with("photo_batch = 5\ntimezone = \"UTC0\"\n"), "{out}");
        let out = set("city = \"x\"", "photo_batch", Some(&Value::Num("3".into()))).unwrap();
        assert_eq!(out, "city = \"x\"\nphoto_batch = 3\n", "a file without a final newline");
        assert_eq!(set("", "city", Some(&Value::Str("x".into()))).unwrap(), "city = \"x\"\n");
    }

    #[test]
    fn written_text_parses_back() {
        for s in ["תל אביב", "a \"quoted\" \\ back", "tab\there", "Шалом", ""] {
            let out = set(FILE, "place_name", Some(&Value::Str(s.into()))).unwrap();
            assert_eq!(Config::parse(&out).unwrap().place_name.as_deref(), Some(s), "{out}");
        }
        assert_eq!(format_value(&Value::List(vec![])), "[]");
        assert_eq!(format_value(&Value::List(vec!["06:00".into()])), "[\"06:00\"]");
    }

    #[test]
    fn a_broken_file_is_not_rewritten() {
        assert!(set("city = \"x\n", "city", Some(&Value::Str("y".into()))).is_err());
    }

    #[test]
    fn example_config_round_trips() {
        let text = include_str!("../../config.example.toml");
        let mut out = text.to_string();
        for (k, v) in parse_flat(text).unwrap() {
            out = set(&out, &k, Some(&v)).unwrap();
        }
        assert_eq!(Config::parse(&out).unwrap(), Config::parse(text).unwrap());
        let comments = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('#')).count();
        assert_eq!(comments(&out), comments(text) - 1, "only the comment inside the calendars list goes (the list is rewritten)");
    }
}
