//! The settings page the device serves in settings mode (and the emulator,
//! with the same HTML): a form with every setting of `config.toml`, and
//! `handle`, which both web servers call for each request.
//!
//! Secrets never go back to the browser: the Wi-Fi password field is always
//! empty (empty = unchanged) and a secret calendar link (`/private-`) shows
//! as a placeholder line that keeps the stored link.

use crate::config::{self, Config, Value};

const PAGE: &str = include_str!("portal.html");

/// An HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

/// How a setting is edited on the page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Number,
    /// A list, one item per line.
    Lines,
    /// Never shown; empty = unchanged.
    Secret,
    /// `Lines`, with secret links shown as placeholders.
    Calendars,
    /// A checkbox (on by default): the page sends a hidden "off" and, when
    /// ticked, "on" after it.
    Bool,
}

/// Every setting on the page, in its order.
const FIELDS: [(&str, Kind); 14] = [
    ("album_url", Kind::Text),
    ("timezone", Kind::Text),
    ("city", Kind::Text),
    ("place_name", Kind::Text),
    ("latitude", Kind::Number),
    ("longitude", Kind::Number),
    ("calendars", Kind::Calendars),
    ("overlay_opacity", Kind::Number),
    ("wifi_ssid", Kind::Text),
    ("wifi_password", Kind::Secret),
    ("update_times", Kind::Lines),
    ("photo_batch", Kind::Number),
    ("refresh_minutes", Kind::Number),
    ("auto_update", Kind::Bool),
];

const HTML: &str = "text/html; charset=utf-8";

/// Who serves the page.
#[derive(Debug, Clone, Copy)]
pub struct Host<'a> {
    /// The device's rule: settings without Wi-Fi are not saved.
    pub wifi_needed: bool,
    /// The running firmware's version, shown on the page.
    pub firmware: &'a str,
}

/// One request: `GET /` is the page, `POST /save` saves the form. Returns
/// the answer and, after a good save, the new text of the settings file.
/// `config_text` is the file as it is now (empty if there is none).
/// (`POST /update`, the firmware upload, is the server's own.)
pub fn handle(method: &str, path: &str, body: &[u8], config_text: &str, host: &Host) -> (Response, Option<String>) {
    let wifi_needed = host.wifi_needed;
    let path = path.split('?').next().unwrap_or("");
    match (method, path) {
        ("GET", "/" | "/index.html") => {
            let message = match (config::parse_flat(config_text), Config::usable(config_text, wifi_needed)) {
                (_, Ok(_)) => Message::None,
                (Err(e), _) => Message::Error(format!("The settings file cannot be read ({e:#}); saving the form replaces it.")),
                (Ok(_), Err(e)) => Message::Error(format!("The settings are not complete yet: {e:#}")),
            };
            (page(config_text, host, message), None)
        }
        ("POST", "/save") => save(&decode_form(body), config_text, host),
        _ => (Response { status: 404, content_type: "text/plain; charset=utf-8", body: "Not found".into() }, None),
    }
}

/// The answer when a good save could not be stored (e.g. no space left):
/// the form as sent, with the reason; nothing was saved.
pub fn store_failed(new_text: &str, why: &str, host: &Host) -> Response {
    page(new_text, host, Message::Error(format!("Not saved: the device could not store the settings ({why}). Try again.")))
}

enum Message {
    None,
    Ok(String),
    Error(String),
}

/// The form applied to the file; saved only if the result is valid.
fn save(form: &[(String, String)], current: &str, host: &Host) -> (Response, Option<String>) {
    // a file that cannot be read at all is replaced by the form
    let readable = config::parse_flat(current).is_ok();
    let stored = config::parse_flat(current).unwrap_or_default();
    let mut text = if readable { current.to_string() } else { String::new() };
    for &(key, kind) in &FIELDS {
        // a checkbox's last value counts (the hidden "off" comes first)
        let input = if kind == Kind::Bool { form.iter().rev().find(|(k, _)| k == key) } else { form.iter().find(|(k, _)| k == key) };
        let Some(input) = input.map(|(_, v)| v.as_str()) else { continue };
        let value = match kind {
            // on, as by default, and not written yet: the file stays as it is
            Kind::Bool if input == "on" && !stored.iter().any(|(k, _)| k == key) => continue,
            Kind::Bool => Some(Value::Bool(input == "on")),
            Kind::Secret if input.is_empty() => continue,
            Kind::Secret => Some(Value::Str(input.to_string())),
            Kind::Text => Some(input.trim()).filter(|s| !s.is_empty()).map(|s| Value::Str(s.to_string())),
            // not a number: kept as text, so the check names the setting
            Kind::Number => Some(input.trim()).filter(|s| !s.is_empty()).map(|s| match s.parse::<f64>() {
                Ok(_) => Value::Num(s.to_string()),
                Err(_) => Value::Str(s.to_string()),
            }),
            Kind::Lines | Kind::Calendars => {
                let private = private_links(&stored);
                let items: Vec<String> = input
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| match placeholder_number(l) {
                        Some(n) if kind == Kind::Calendars => private.get(n.wrapping_sub(1)).cloned(),
                        _ => Some(l.to_string()),
                    })
                    .collect();
                (!items.is_empty()).then_some(Value::List(items))
            }
        };
        text = match config::set(&text, key, value.as_ref()) {
            Ok(t) => t,
            Err(e) => return (page(current, host, Message::Error(format!("{e:#}"))), None),
        };
    }
    match Config::usable(&text, host.wifi_needed) {
        Ok(_) => (page(&text, host, Message::Ok("Saved — press the green button on the device to finish.".into())), Some(text)),
        Err(e) => (page(&text, host, Message::Error(format!("Not saved: {e:#}"))), None),
    }
}

/// The stored secret calendar links, in order.
fn private_links(entries: &[(String, Value)]) -> Vec<String> {
    match entries.iter().find(|(k, _)| k == "calendars") {
        Some((_, Value::List(l))) => l.iter().filter(|u| is_private(u)).cloned().collect(),
        _ => Vec::new(),
    }
}

fn is_private(url: &str) -> bool {
    url.contains("/private-")
}

fn placeholder(n: usize) -> String {
    format!("private calendar {n} (unchanged)")
}

/// `n` of a "private calendar n (unchanged)" line.
fn placeholder_number(line: &str) -> Option<usize> {
    line.strip_prefix("private calendar ")?.strip_suffix(" (unchanged)")?.parse().ok()
}

/// The page with the settings of `text` filled in.
fn page(text: &str, host: &Host, message: Message) -> Response {
    let entries = config::parse_flat(text).unwrap_or_default();
    let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let field = |key: &str| -> String {
        let kind = FIELDS.iter().find(|(k, _)| *k == key).map(|(_, kind)| *kind);
        match (kind, get(key)) {
            (Some(Kind::Secret), _) | (_, None) => String::new(),
            (Some(Kind::Calendars), Some(Value::List(l))) => {
                let mut n = 0;
                let lines: Vec<String> = l
                    .iter()
                    .map(|u| {
                        if is_private(u) {
                            n += 1;
                            placeholder(n)
                        } else {
                            u.clone()
                        }
                    })
                    .collect();
                lines.join("\n")
            }
            (_, Some(Value::List(l))) => l.join("\n"),
            (_, Some(Value::Str(s) | Value::Num(s))) => s.clone(),
            (_, Some(Value::Bool(b))) => b.to_string(),
        }
    };
    let (class, msg) = match &message {
        Message::None => ("", ""),
        Message::Ok(m) => ("ok", m.as_str()),
        Message::Error(m) => ("error", m.as_str()),
    };
    let body = fill(PAGE, |name| match name {
        "message" => html_escape(msg),
        "message_class" => class.to_string(),
        "firmware_version" => html_escape(host.firmware),
        "auto_update_checked" => if matches!(get("auto_update"), Some(Value::Bool(false))) { "" } else { "checked" }.to_string(),
        "wifi_password_hint" => if get("wifi_password").is_some() { "unchanged" } else { "not set" }.to_string(),
        key => html_escape(&field(key)),
    });
    Response { status: 200, content_type: HTML, body }
}

/// `template` with every `{{name}}` replaced, in one pass (a value that
/// contains `{{…}}` stays as it is).
fn fill(template: &str, value: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(template.len() + 1024);
    let mut rest = template;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j) => {
                out.push_str(&value(&after[..j]));
                rest = &after[j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `application/x-www-form-urlencoded` pairs (`+` is a space).
pub fn decode_form(body: &[u8]) -> Vec<(String, String)> {
    let decode = |s: &str| crate::url::percent_decode(&s.replace('+', " "));
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// Text safe inside HTML, also inside attribute values.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# my frame\n\
        album_url = \"https://photos.app.goo.gl/abc\"\n\
        city = \"Tel Aviv\"\n\
        calendars = [\n  \"https://calendar.google.com/calendar/ical/me/private-SECRET1/basic.ics\",\n  \"https://calendar.google.com/calendar/ical/hol/public/basic.ics\",\n]\n\
        wifi_ssid = \"home\"\n\
        wifi_password = \"PASSWORD1\"\n\
        # photo_batch = 10\n";

    fn host(wifi_needed: bool) -> Host<'static> {
        Host { wifi_needed, firmware: "85" }
    }

    fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
        pairs.iter().map(|(k, v)| format!("{k}={}", crate::url::percent_encode(v))).collect::<Vec<_>>().join("&").into_bytes()
    }

    /// The form as the browser would send the page back unchanged.
    fn unchanged() -> Vec<(&'static str, &'static str)> {
        vec![
            ("album_url", "https://photos.app.goo.gl/abc"),
            ("timezone", ""),
            ("city", "Tel Aviv"),
            ("place_name", ""),
            ("latitude", ""),
            ("longitude", ""),
            ("calendars", "private calendar 1 (unchanged)\r\nhttps://calendar.google.com/calendar/ical/hol/public/basic.ics"),
            ("overlay_opacity", ""),
            ("wifi_ssid", "home"),
            ("wifi_password", ""),
            ("update_times", ""),
            ("photo_batch", ""),
            ("refresh_minutes", ""),
        ]
    }

    fn save(pairs: &[(&str, &str)]) -> (Response, Option<String>) {
        handle("POST", "/save", &form(pairs), FILE, &host(false))
    }

    #[test]
    fn the_page_shows_the_settings_but_no_secrets() {
        let (r, saved) = handle("GET", "/", b"", FILE, &host(false));
        assert_eq!((r.status, r.content_type, saved), (200, "text/html; charset=utf-8", None));
        assert!(r.body.contains("value=\"https://photos.app.goo.gl/abc\""));
        assert!(r.body.contains("value=\"Tel Aviv\""));
        assert!(r
            .body
            .contains("private calendar 1 (unchanged)\nhttps://calendar.google.com/calendar/ical/hol/public/basic.ics</textarea>"));
        assert!(!r.body.contains("SECRET1") && !r.body.contains("PASSWORD1"), "no secrets in the page");
        assert!(!r.body.contains("{{"), "every placeholder filled");
    }

    #[test]
    fn saving_the_page_unchanged_keeps_the_file() {
        let (r, saved) = save(&unchanged());
        assert_eq!(r.status, 200);
        assert_eq!(saved.as_deref(), Some(FILE), "same settings, same file");
        assert!(r.body.contains("Saved"));
    }

    #[test]
    fn saving_valid_form_writes_the_file_keeping_comments() {
        let mut f = unchanged();
        f[2] = ("city", "Haifa");
        f[11] = ("photo_batch", "5");
        f[4] = ("latitude", "32.8");
        f[5] = ("longitude", "35.0");
        let (r, saved) = save(&f);
        let saved = saved.expect("saved");
        assert!(saved.starts_with("# my frame\n"));
        assert!(saved.contains("city = \"Haifa\"\n"));
        assert!(saved.contains("# photo_batch = 10\nphoto_batch = 5\n"), "{saved}");
        let c = Config::parse(&saved).unwrap();
        assert_eq!((c.photo_batch, c.coordinates()), (5, Some((32.8, 35.0))));
        assert!(r.body.contains("press the green button"), "{}", r.body);
    }

    #[test]
    fn an_invalid_form_saves_nothing_and_says_why() {
        let mut f = unchanged();
        f[11] = ("photo_batch", "0");
        let (r, saved) = save(&f);
        assert_eq!(saved, None);
        assert_eq!(r.status, 200, "the page again, with the message");
        assert!(r.body.contains("class=\"msg error\"") && r.body.contains("photo_batch"), "{}", r.body);
        f[11] = ("photo_batch", "five");
        assert_eq!(save(&f).1, None, "not a number");
        f[11] = ("photo_batch", "");
        f[4] = ("latitude", "32.8");
        assert_eq!(save(&f).1, None, "latitude without longitude");
    }

    #[test]
    fn empty_password_keeps_the_stored_one() {
        let saved = save(&unchanged()).1.unwrap();
        assert_eq!(Config::parse(&saved).unwrap().wifi_password.as_deref(), Some("PASSWORD1"));
        let mut f = unchanged();
        f[9] = ("wifi_password", "new pass");
        let saved = save(&f).1.unwrap();
        assert_eq!(Config::parse(&saved).unwrap().wifi_password.as_deref(), Some("new pass"));
    }

    #[test]
    fn private_calendar_placeholders_keep_their_links() {
        let mut f = unchanged();
        f[6] = ("calendars", "https://calendar.google.com/calendar/ical/new/public/basic.ics\nprivate calendar 1 (unchanged)\n\n");
        let c = Config::parse(&save(&f).1.unwrap()).unwrap();
        assert_eq!(
            c.calendars,
            [
                "https://calendar.google.com/calendar/ical/new/public/basic.ics",
                "https://calendar.google.com/calendar/ical/me/private-SECRET1/basic.ics"
            ]
        );
        f[6] = ("calendars", "");
        assert!(Config::parse(&save(&f).1.unwrap()).unwrap().calendars.is_empty(), "all removed");
    }

    #[test]
    fn empty_optional_fields_remove_their_key() {
        let mut f = unchanged();
        f[3] = ("place_name", "Home");
        let with = handle("POST", "/save", &form(&f), FILE, &host(false)).1.unwrap();
        assert!(with.contains("place_name = \"Home\""));
        f[3] = ("place_name", "  ");
        let without = handle("POST", "/save", &form(&f), &with, &host(false)).1.unwrap();
        assert!(!without.contains("place_name ="), "{without}");
    }

    #[test]
    fn a_broken_or_missing_file_still_opens_the_page() {
        for text in ["", "city = \"unclosed\n", "album_url = \"https://photos.app.goo.gl/abc\"\n"] {
            let (r, _) = handle("GET", "/", b"", text, &host(false));
            assert_eq!(r.status, 200);
            assert!(r.body.contains("<form"), "{text:?}");
            assert!(r.body.contains("class=\"msg error\""), "says what is wrong: {text:?}");
        }
        let mut f = unchanged();
        f[6] = ("calendars", "");
        f[9] = ("wifi_password", "pw");
        let saved = handle("POST", "/save", &form(&f), "city = \"unclosed\n", &host(false)).1.expect("a good form replaces a broken file");
        assert_eq!(Config::parse(&saved).unwrap().city, "Tel Aviv");
    }

    #[test]
    fn the_page_refuses_settings_without_wifi_on_the_device() {
        let no_wifi = "album_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";
        let (r, _) = handle("GET", "/", b"", no_wifi, &host(true));
        assert!(r.body.contains("not complete yet") && r.body.contains("wifi_ssid"), "{}", r.body);
        let f: Vec<_> = unchanged().into_iter().map(|(k, v)| (k, if k == "wifi_ssid" { "" } else { v })).collect();
        let (r, saved) = handle("POST", "/save", &form(&f), FILE, &host(true));
        assert!(saved.is_none() && r.body.contains("Not saved") && r.body.contains("wifi_ssid"), "{}", r.body);
        assert!(handle("POST", "/save", &form(&f), FILE, &host(false)).1.is_some(), "the emulator does without");
        assert!(handle("POST", "/save", &form(&unchanged()), FILE, &host(true)).1.is_some());
    }

    #[test]
    fn a_save_the_device_could_not_store_says_so_and_keeps_the_form() {
        let (_, saved) = save(&unchanged().into_iter().map(|(k, v)| (k, if k == "city" { "Haifa" } else { v })).collect::<Vec<_>>());
        let r = store_failed(&saved.unwrap(), "no space", &host(true));
        assert!(r.body.contains("Not saved") && r.body.contains("no space") && !r.body.contains("Saved —"), "{}", r.body);
        assert!(r.body.contains("value=\"Haifa\""), "the form keeps what was typed");
        assert!(!r.body.contains("PASSWORD1") && !r.body.contains("SECRET1"));
    }

    #[test]
    fn the_page_shows_the_firmware_and_the_upload_box() {
        let (r, _) = handle("GET", "/", b"", FILE, &Host { wifi_needed: true, firmware: "85" });
        assert!(r.body.contains("frogterm 85"), "the running version");
        assert!(r.body.contains("type=\"file\"") && r.body.contains("/update"), "the upload box");
        let (r, _) = handle("GET", "/", b"", FILE, &Host { wifi_needed: false, firmware: "<x>" });
        assert!(r.body.contains("frogterm &lt;x&gt;"), "escaped");
    }

    #[test]
    fn auto_update_is_a_checkbox_on_by_default() {
        let (r, _) = handle("GET", "/", b"", FILE, &host(true));
        assert!(r.body.contains("name=\"auto_update\" value=\"on\" checked"), "on when not set");
        // the browser sends the hidden "off" and, when ticked, "on" after it
        let ticked = [unchanged(), vec![("auto_update", "off"), ("auto_update", "on")]].concat();
        assert_eq!(save(&ticked).1.unwrap(), FILE, "on and not set: the file stays as it is");
        let off = [unchanged(), vec![("auto_update", "off")]].concat();
        let text = save(&off).1.unwrap();
        assert!(text.contains("auto_update = false"), "{text}");
        let (r, _) = handle("GET", "/", b"", &text, &host(true));
        assert!(r.body.contains("name=\"auto_update\" value=\"on\" >"), "shown off");
        let on_again = handle("POST", "/save", &form(&ticked), &text, &host(true)).1.unwrap();
        assert!(on_again.contains("auto_update = true"), "{on_again}");
    }

    #[test]
    fn values_are_html_escaped() {
        assert_eq!(html_escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
        let text = format!("{FILE}place_name = \"\\\"><script>\"\n");
        let (r, _) = handle("GET", "/", b"", &text, &host(false));
        assert!(!r.body.contains("\"><script>") && r.body.contains("&quot;&gt;&lt;script&gt;"), "the injected value is escaped");
    }

    #[test]
    fn form_bodies_decode() {
        assert_eq!(
            decode_form(b"a=1+2&b=%D7%AA%26x&c=&d"),
            [("a".into(), "1 2".into()), ("b".into(), "ת&x".into()), ("c".into(), String::new()), ("d".into(), String::new())]
        );
    }

    #[test]
    fn unknown_path_is_404() {
        assert_eq!(handle("GET", "/favicon.ico", b"", FILE, &host(false)).0.status, 404);
        assert_eq!(handle("POST", "/", b"", FILE, &host(false)).0.status, 404);
        assert_eq!(handle("GET", "/?x=1", b"", FILE, &host(false)).0.status, 200, "a query is ignored");
    }
}
