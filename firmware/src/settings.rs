//! The settings: the text of a `config.toml` in NVS (namespace `frogterm`,
//! blob `config`), written by the settings page and read at every wake.
//! With the `seed-config` feature an empty NVS gets the `config.toml` the
//! firmware was built with.

use anyhow::{Context, Result};
use esp_idf_svc::nvs::{EspNvs, NvsDefault};
use frogterm_core::config::Config;

const BLOB: &str = "config";

/// The stored settings text, if there is one.
pub fn load_text(nvs: &EspNvs<NvsDefault>) -> Option<String> {
    let len = nvs.blob_len(BLOB).ok()??;
    let mut buf = vec![0u8; len];
    let bytes = nvs.get_blob(BLOB, &mut buf).ok()??;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

#[allow(dead_code)] // used by the settings page (next step)
pub fn save_text(nvs: &EspNvs<NvsDefault>, text: &str) -> Result<()> {
    nvs.set_blob(BLOB, text.as_bytes()).context("store the settings in NVS")
}

/// The stored settings if the device can run on them (the reason is logged
/// otherwise; never the text, which holds secrets).
pub fn load(nvs: &EspNvs<NvsDefault>) -> Option<Config> {
    seed(nvs);
    let Some(text) = load_text(nvs) else {
        log::warn!("no settings stored");
        return None;
    };
    Config::usable(&text, true).map_err(|e| log::warn!("the stored settings cannot be used: {e:#}")).ok()
}

/// An empty NVS gets the settings the firmware was built with.
#[cfg(feature = "seed-config")]
fn seed(nvs: &EspNvs<NvsDefault>) {
    if matches!(nvs.blob_len(BLOB), Ok(Some(_))) {
        return;
    }
    match save_text(nvs, include_str!(concat!(env!("OUT_DIR"), "/seed.toml"))) {
        Ok(()) => log::info!("settings seeded from the build's config.toml"),
        Err(e) => log::warn!("{e:#}"),
    }
}

#[cfg(not(feature = "seed-config"))]
fn seed(_: &EspNvs<NvsDefault>) {}

/// Host part of a URL, for logs (links contain secrets).
pub fn host(url: &str) -> &str {
    url.split('/').nth(2).unwrap_or("?")
}
