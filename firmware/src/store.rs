//! Raw flash partitions: "photos" (album photo list, record = b"FTL1" + u32
//! length + bytes, rewritten weekly) and "screens" (ready-to-show photo
//! screens, one 192 KB slot each, rewritten once per batch).
//!
//! Wear: NOR flash takes ~100 000 erases per sector; each screen slot is
//! erased once per batch (~once a day), the photo list once a week.

use std::ffi::CString;

use anyhow::{anyhow, bail, Result};
use esp_idf_svc::sys;
use frogterm_core::framebuf::FrameBuffer;
use frogterm_core::photo_list::PhotoList;

const MAGIC: &[u8; 4] = b"FTL1";
const SECTOR: usize = 4096;
/// One screen slot: 192 000 bytes rounded up to 64 KB blocks.
const SCREEN_SLOT: usize = 0x30000;
const SCREEN_BYTES: usize = 192_000;
/// "undefined" data subtype (0x06); partitions are found by label.
const SUBTYPE: u32 = 0x06;

pub struct Store;

impl Store {
    pub fn load_list(&self) -> Option<PhotoList> {
        let bytes = load("photos", SUBTYPE)?;
        PhotoList::from_text(&String::from_utf8_lossy(&bytes))
    }

    pub fn save_list(&mut self, list: &PhotoList) -> Result<()> {
        save("photos", SUBTYPE, list.to_text().as_bytes())
    }

    /// Screen slot `slot` (invalid or never written -> `None`).
    pub fn load_screen(&self, slot: usize) -> Option<FrameBuffer> {
        let p = partition("screens", SUBTYPE)?;
        let offset = slot.checked_mul(SCREEN_SLOT)?;
        if offset + SCREEN_BYTES > p.size as usize {
            return None;
        }
        let mut data = vec![0u8; SCREEN_BYTES];
        unsafe {
            if sys::esp_partition_read(p, offset, data.as_mut_ptr().cast(), SCREEN_BYTES) != sys::ESP_OK {
                return None;
            }
        }
        FrameBuffer::from_bytes(&data)
    }

    pub fn save_screen(&mut self, slot: usize, fb: &FrameBuffer) -> Result<()> {
        let p = partition("screens", SUBTYPE).ok_or_else(|| anyhow!("partition screens missing (flash with partitions.csv)"))?;
        let offset = slot * SCREEN_SLOT;
        if offset + SCREEN_SLOT > p.size as usize {
            bail!("screen slot {slot} does not fit");
        }
        let bytes = fb.as_bytes();
        unsafe {
            // slots are 64 KB aligned, so this is 3 fast block erases
            sys::esp!(sys::esp_partition_erase_range(p, offset, SCREEN_SLOT))?;
            sys::esp!(sys::esp_partition_write(p, offset, bytes.as_ptr().cast(), bytes.len()))?;
        }
        Ok(())
    }
}

fn partition(label: &str, subtype: u32) -> Option<&'static sys::esp_partition_t> {
    let label = CString::new(label).ok()?;
    unsafe { sys::esp_partition_find_first(sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA, subtype, label.as_ptr()).as_ref() }
}

fn load(label: &str, subtype: u32) -> Option<Vec<u8>> {
    let p = partition(label, subtype)?;
    let mut head = [0u8; 8];
    unsafe {
        if sys::esp_partition_read(p, 0, head.as_mut_ptr().cast(), 8) != sys::ESP_OK {
            return None;
        }
    }
    if &head[..4] != MAGIC {
        return None;
    }
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len + 8 > p.size as usize {
        return None;
    }
    let mut data = vec![0u8; len];
    unsafe {
        if sys::esp_partition_read(p, 8, data.as_mut_ptr().cast(), len) != sys::ESP_OK {
            return None;
        }
    }
    Some(data)
}

fn save(label: &str, subtype: u32, bytes: &[u8]) -> Result<()> {
    let p = partition(label, subtype).ok_or_else(|| anyhow!("partition {label} missing (flash with partitions.csv)"))?;
    let total = bytes.len() + 8;
    if total > p.size as usize {
        bail!("{} bytes do not fit into partition {label}", bytes.len());
    }
    let erase = total.div_ceil(SECTOR) * SECTOR;
    let mut head = [0u8; 8];
    head[..4].copy_from_slice(MAGIC);
    head[4..].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    unsafe {
        sys::esp!(sys::esp_partition_erase_range(p, 0, erase))?;
        // data first, header last: a power cut leaves no valid-looking half record
        sys::esp!(sys::esp_partition_write(p, 8, bytes.as_ptr().cast(), bytes.len()))?;
        sys::esp!(sys::esp_partition_write(p, 0, head.as_ptr().cast(), 8))?;
    }
    Ok(())
}
