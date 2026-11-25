//! Bakes `../config.toml` into the firmware as `FT_*` environment variables,
//! validated with the same code as the PC tools.

#[path = "../pc/src/config.rs"]
#[allow(dead_code)]
mod config;

fn main() -> anyhow::Result<()> {
    embuild::espidf::sysenv::output();
    println!("cargo:rerun-if-changed=../config.toml");
    println!("cargo:rerun-if-changed=../pc/src/config.rs");
    let c = config::Config::load(std::path::Path::new("../config.toml"))?;
    let ssid = c.wifi_ssid.clone().filter(|s| !s.is_empty());
    let Some(ssid) = ssid else { anyhow::bail!("wifi_ssid missing in config.toml") };
    let password = c.wifi_password.clone().unwrap_or_default();
    let (lat, lon) = c.coordinates().map_or((String::new(), String::new()), |(a, b)| (a.to_string(), b.to_string()));
    for (k, v) in [
        ("FT_ALBUM_URL", c.album_url.clone()),
        ("FT_CITY", c.city.clone()),
        ("FT_PLACE", c.label().to_string()),
        ("FT_LAT", lat),
        ("FT_LON", lon),
        ("FT_CALENDARS", c.calendars.join("\n")),
        ("FT_OPACITY", c.overlay_opacity.to_string()),
        ("FT_WIFI_SSID", ssid),
        ("FT_WIFI_PASSWORD", password),
        ("FT_UPDATE_HOUR", c.update_hour.to_string()),
        ("FT_PHOTO_BATCH", c.photo_batch.to_string()),
        ("FT_REFRESH_MINUTES", c.refresh_minutes.to_string()),
    ] {
        // newlines are not allowed in rustc-env values: escape them
        println!("cargo:rustc-env={k}={}", v.replace('\n', "\\n"));
    }
    Ok(())
}
