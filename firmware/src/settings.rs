//! The settings: the text of a `config.toml` in NVS (namespace `frogterm`,
//! blob `config`), written by the settings page and read at every wake.
//! Built with the `copy-config` feature (`make flash`), the first wake after
//! flashing replaces them with the `config.toml` the firmware was built with.

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

pub fn save_text(nvs: &EspNvs<NvsDefault>, text: &str) -> Result<()> {
    nvs.set_blob(BLOB, text.as_bytes()).context("store the settings in NVS")
}

/// The stored settings if the device can run on them (the reason is logged
/// otherwise; never the text, which holds secrets).
pub fn load(nvs: &EspNvs<NvsDefault>) -> Option<Config> {
    copy_flashed(nvs);
    let Some(text) = load_text(nvs) else {
        log::warn!("no settings stored");
        return None;
    };
    Config::usable(&text, true).map_err(|e| log::warn!("the stored settings cannot be used: {e:#}")).ok()
}

/// The settings flashed with this firmware, once per flash (the id of the
/// flash is kept in NVS): afterwards the settings page edits them.
#[cfg(feature = "copy-config")]
fn copy_flashed(nvs: &EspNvs<NvsDefault>) {
    const ID_KEY: &str = "config_flash";
    const ID: &str = env!("FT_FLASH_ID");
    let mut buf = [0u8; 32];
    if nvs.get_str(ID_KEY, &mut buf).ok().flatten() == Some(ID) {
        return;
    }
    match save_text(nvs, include_str!(concat!(env!("OUT_DIR"), "/config.toml"))) {
        Ok(()) => {
            if let Err(e) = nvs.set_str(ID_KEY, ID) {
                log::warn!("could not note the flashed settings as copied: {e}");
            }
            log::info!("settings copied from the flashed config.toml");
        }
        Err(e) => log::warn!("{e:#}"),
    }
}

#[cfg(not(feature = "copy-config"))]
fn copy_flashed(_: &EspNvs<NvsDefault>) {}

/// Host part of a URL, for logs (links contain secrets).
pub fn host(url: &str) -> &str {
    url.split('/').nth(2).unwrap_or("?")
}
