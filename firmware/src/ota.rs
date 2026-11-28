//! Firmware updates: frogterm's version tag (which every update check looks
//! for), installing a firmware into the other app slot piece by piece (an
//! upload on the settings page, or a newer release at a sync), and
//! confirming a new firmware once it works (else the bootloader goes back to
//! the previous one at the next restart).

use anyhow::{anyhow, bail, Result};
use esp_idf_svc::nvs::{EspNvs, NvsDefault};
use esp_idf_svc::sys::{self, esp};
use frogterm_core::firmware::{self, TagScanner, UPDATE_SITE};

use crate::net::{HttpStatus, Net};

/// In every frogterm firmware; read at start so the linker keeps it.
static VERSION_TAG: &str = concat!("<frogterm-version:", env!("FT_VERSION"), ">");
/// NVS: the release an automatic update installed last (not tried again if
/// it was rolled back).
const TRIED_KEY: &str = "fw_tried";

/// This firmware's version (`FT_VERSION`: commits pushed to GitHub, or
/// "dev" for a plain cargo build).
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

/// A firmware being written into the other app slot, piece by piece. It is
/// checked as it comes (refused before anything is erased if its start is
/// wrong) and becomes the one to boot only with `finish`; dropped before,
/// nothing changes.
pub struct Install {
    /// The size, if known (an upload); a download erases the whole slot.
    size: Option<usize>,
    /// The start, until `check_header` has enough of it.
    head: Vec<u8>,
    ota: Option<(sys::esp_ota_handle_t, *const sys::esp_partition_t)>,
    written: usize,
    scanner: TagScanner,
}

impl Install {
    pub fn new(size: Option<usize>) -> Result<Install> {
        match size {
            Some(0) => bail!("no file came"),
            Some(n) if n > firmware::MAX_IMAGE => {
                bail!("this file is larger than the {} MB a firmware may have", firmware::MAX_IMAGE / (1024 * 1024))
            }
            _ => Ok(Install { size, head: Vec::new(), ota: None, written: 0, scanner: TagScanner::new() }),
        }
    }

    pub fn feed(&mut self, piece: &[u8]) -> Result<()> {
        self.scanner.feed(piece);
        if self.ota.is_some() {
            return self.write(piece);
        }
        self.head.extend_from_slice(piece);
        if self.head.len() < firmware::HEADER_LEN {
            return Ok(());
        }
        firmware::check_header(&self.head)?;
        // SAFETY: plain ESP-IDF calls; the partition pointer stays valid
        let part = unsafe { sys::esp_ota_get_next_update_partition(std::ptr::null()) };
        if part.is_null() {
            bail!("this device needs one USB flash (make flash) before it can install firmware itself");
        }
        let mut handle: sys::esp_ota_handle_t = 0;
        let size = self.size.unwrap_or(sys::OTA_SIZE_UNKNOWN as usize);
        esp!(unsafe { sys::esp_ota_begin(part, size, &mut handle) }).map_err(|e| anyhow!("could not start the update ({e})"))?;
        self.ota = Some((handle, part));
        let head = std::mem::take(&mut self.head);
        self.write(&head)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let Some((handle, _)) = self.ota else { return Ok(()) };
        self.written += bytes.len();
        if self.written > self.size.unwrap_or(firmware::MAX_IMAGE) {
            bail!("the firmware is larger than it should be");
        }
        esp!(unsafe { sys::esp_ota_write(handle, bytes.as_ptr().cast(), bytes.len()) })
            .map_err(|e| anyhow!("writing the firmware failed ({e})"))
    }

    /// The whole firmware came: checked (checksum, SHA-256, frogterm's tag)
    /// and made the one to boot. Returns its version.
    pub fn finish(mut self) -> Result<String> {
        let complete = self.ota.is_some() && self.size.is_none_or(|n| n == self.written);
        if !complete {
            bail!("the firmware was cut off");
        }
        let Some(version) = self.scanner.version().map(str::to_string) else { bail!("this is not frogterm firmware") };
        let Some((handle, part)) = self.ota.take() else { bail!("the firmware was cut off") };
        // esp_ota_end frees the handle in any case
        esp!(unsafe { sys::esp_ota_end(handle) }).map_err(|e| anyhow!("the firmware did not pass the device's check ({e})"))?;
        esp!(unsafe { sys::esp_ota_set_boot_partition(part) }).map_err(|e| anyhow!("could not switch to the new firmware ({e})"))?;
        Ok(version)
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if let Some((handle, _)) = self.ota.take() {
            // SAFETY: an update begun and not ended: drop it
            unsafe { sys::esp_ota_abort(handle) };
        }
    }
}

/// An upload of `len` bytes read with `read` (the settings page).
pub fn install(len: usize, read: &mut dyn FnMut(&mut [u8]) -> Result<usize>, progress: &mut dyn FnMut()) -> Result<String> {
    let mut install = Install::new(Some(len))?;
    let mut buf = vec![0u8; 4096];
    let mut done = 0;
    while done < len {
        let n = read(&mut buf[..(len - done).min(4096)])?;
        if n == 0 {
            bail!("the upload was cut off");
        }
        install.feed(&buf[..n])?;
        done += n;
        progress();
    }
    install.finish()
}

/// At a sync (`auto_update` on): installs the newest release if it is newer
/// than this firmware. True if one was installed (the caller restarts into
/// it at once); a failed check never fails the sync.
pub fn auto_update(net: &mut Net, nvs: &EspNvs<NvsDefault>) -> bool {
    match update_from_site(net, nvs) {
        Ok(Some(v)) => {
            log::info!("firmware {v} installed");
            true
        }
        Ok(None) => false,
        Err(e) => {
            log::warn!("firmware update check: {e:#}");
            false
        }
    }
}

/// Starts the firmware that was just installed.
pub fn restart() -> ! {
    log::info!("restarting into the new firmware");
    // SAFETY: plain ESP-IDF restart
    unsafe { sys::esp_restart() }
}

fn update_from_site(net: &mut Net, nvs: &EspNvs<NvsDefault>) -> Result<Option<String>> {
    let running = version();
    if running.parse::<u32>().is_err() {
        log::info!("firmware {running} has no release number: no automatic update");
        return Ok(None);
    }
    let mut text = Vec::new();
    let r = net.get(&format!("{UPDATE_SITE}version.txt"), &mut |c| {
        if text.len() < 64 {
            text.extend_from_slice(c);
        }
        Ok(())
    })?;
    if r.status != 200 {
        bail!(HttpStatus(r.status));
    }
    let latest = String::from_utf8_lossy(&text).trim().to_string();
    if !firmware::newer(running, &latest) {
        log::info!("firmware {running} is the newest (published: {latest})");
        return Ok(None);
    }
    let mut buf = [0u8; 16];
    if nvs.get_str(TRIED_KEY, &mut buf).ok().flatten() == Some(latest.as_str()) {
        log::warn!("release {latest} was installed before and did not work: not again");
        return Ok(None);
    }
    log::info!("release {latest} is out (this is {running}): installing");
    let mut install = Install::new(None)?;
    let r = net.get(&format!("{UPDATE_SITE}firmware.bin"), &mut |c| install.feed(c))?;
    if r.status != 200 {
        bail!(HttpStatus(r.status));
    }
    let version = install.finish()?;
    // a release that rolls back is not downloaded again at every sync
    if let Err(e) = nvs.set_str(TRIED_KEY, &version) {
        log::warn!("could not note the installed release: {e}");
    }
    Ok(Some(version))
}
