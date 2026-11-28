//! Firmware updates from the settings page: frogterm's version tag (which
//! the page's upload check looks for), installing an uploaded firmware into
//! the other app slot, and confirming a new firmware once it works (else the
//! bootloader goes back to the previous one at the next restart).

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::sys;
use frogterm_core::firmware::{self, TagScanner};

/// In every frogterm firmware; read at start so the linker keeps it.
static VERSION_TAG: &str = concat!("<frogterm-version:", env!("FT_VERSION"), ">");

/// This firmware's version (`FT_VERSION`: commits on main, or "dev").
pub fn version() -> &'static str {
    let tag = core::hint::black_box(VERSION_TAG);
    tag.trim_start_matches("<frogterm-version:").trim_end_matches('>')
}

/// This firmware works (it reached a planned deep sleep): keep it. A new
/// firmware that never gets here is rolled back at the next restart.
pub fn confirm() {
    // SAFETY: plain ESP-IDF call; an error only means nothing to confirm
    // (e.g. a flash layout without update slots)
    unsafe {
        sys::esp_ota_mark_app_valid_cancel_rollback();
    }
}

/// Logs an update that was thrown out (its firmware did not confirm itself).
pub fn log_rollback() {
    // SAFETY: returns a pointer into the partition table, or null
    if let Some(p) = unsafe { sys::esp_ota_get_last_invalid_partition().as_ref() } {
        // SAFETY: the label is a NUL-terminated C string inside the struct
        let label = unsafe { std::ffi::CStr::from_ptr(p.label.as_ptr()) }.to_string_lossy();
        log::warn!("a firmware update in {label} did not work and was rolled back");
    }
}

/// Writes an uploaded firmware of `len` bytes, read with `read`, into the
/// other app slot and makes it the one to boot. Returns its version. On any
/// error nothing changes: the running firmware stays the one that boots.
pub fn install(len: usize, read: &mut dyn FnMut(&mut [u8]) -> Result<usize>, progress: &mut dyn FnMut()) -> Result<String> {
    if len == 0 {
        bail!("no file came");
    }
    if len > firmware::MAX_IMAGE {
        bail!("this file is larger than the {} MB a firmware may have", firmware::MAX_IMAGE / (1024 * 1024));
    }
    let mut buf = vec![0u8; 4096];
    // the start first: refuse a wrong file before erasing anything
    let mut head = Vec::new();
    while head.len() < firmware::HEADER_LEN.min(len) {
        let n = read(&mut buf[..(len - head.len()).min(4096)])?;
        if n == 0 {
            bail!("the upload was cut off");
        }
        head.extend_from_slice(&buf[..n]);
    }
    firmware::check_header(&head)?;
    let mut ota = EspOta::new().context("firmware update")?;
    let mut update = ota
        .initiate_update_with_known_size(len)
        .map_err(|e| anyhow!("this device needs one USB flash (make flash) before it can install firmware from this page ({e})"))?;
    let mut scanner = TagScanner::new();
    let written = (|| -> Result<()> {
        scanner.feed(&head);
        update.write(&head)?;
        let mut done = head.len();
        while done < len {
            let n = read(&mut buf[..(len - done).min(4096)])?;
            if n == 0 {
                bail!("the upload was cut off");
            }
            scanner.feed(&buf[..n]);
            update.write(&buf[..n])?;
            done += n;
            progress();
        }
        if scanner.version().is_none() {
            bail!("this is not frogterm firmware");
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = update.abort();
        return Err(e);
    }
    // checks the whole image (checksum, SHA-256) and makes it the one to boot
    update.complete().map_err(|e| anyhow!("the firmware did not pass the device's check ({e})"))?;
    Ok(scanner.version().unwrap_or("?").to_string())
}
