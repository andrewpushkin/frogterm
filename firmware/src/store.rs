//! Raw flash partitions: "photos" (album photo list, record = b"FTL1" + u32
//! length + bytes, rewritten weekly) and "screens" (ready-to-show photo
//! screens, one 192 KB slot each, rewritten once per batch).
//!
//! Wear: NOR flash takes ~100 000 erases per sector; each screen slot is
//! erased once per batch (~once a day), the photo list once a week.

use anyhow::{anyhow, bail, Result};
use esp_idf_svc::partition::EspPartition;
use frogterm_core::framebuf::FrameBuffer;
use frogterm_core::photo_list::PhotoList;

const MAGIC: &[u8; 4] = b"FTL1";
const SECTOR: usize = 4096;
/// One screen slot: 192 000 bytes rounded up to 64 KB blocks.
const SCREEN_SLOT: usize = 0x30000;
const SCREEN_BYTES: usize = 192_000;

pub struct Store;

impl Store {
    pub fn load_list(&self) -> Option<PhotoList> {
        let bytes = load("photos")?;
        PhotoList::from_text(&String::from_utf8_lossy(&bytes))
    }

    pub fn save_list(&mut self, list: &PhotoList) -> Result<()> {
        save("photos", list.to_text().as_bytes())
    }

    /// Screen slot `slot` (invalid or never written -> `None`).
    pub fn load_screen(&self, slot: usize) -> Option<FrameBuffer> {
        let mut p = partition("screens")?;
        let offset = slot.checked_mul(SCREEN_SLOT)?;
        if offset + SCREEN_BYTES > p.size() {
            return None;
        }
        let mut data = vec![0u8; SCREEN_BYTES];
        p.read(offset, &mut data).ok()?;
        FrameBuffer::from_vec(data)
    }

    pub fn save_screen(&mut self, slot: usize, fb: &FrameBuffer) -> Result<()> {
        let mut p = partition("screens").ok_or_else(|| anyhow!("partition screens missing (flash with partitions.csv)"))?;
        let offset = slot * SCREEN_SLOT;
        if offset + SCREEN_SLOT > p.size() {
            bail!("screen slot {slot} does not fit");
        }
        // slots are 64 KB aligned, so this is 3 fast block erases
        p.erase(offset, SCREEN_SLOT)?;
        p.write(offset, fb.as_bytes())?;
        Ok(())
    }
}

/// The data partition `label` (from partitions.csv).
fn partition(label: &str) -> Option<EspPartition> {
    // SAFETY: each call opens its own short-lived handle, and no two handles
    // of the same partition are used at the same time (the photo list is read
    // in one thread and written later in the same wake, after that thread ended).
    unsafe { EspPartition::new(label) }.ok().flatten()
}

fn load(label: &str) -> Option<Vec<u8>> {
    let mut p = partition(label)?;
    let mut head = [0u8; 8];
    p.read(0, &mut head).ok()?;
    if &head[..4] != MAGIC {
        return None;
    }
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len + 8 > p.size() {
        return None;
    }
    let mut data = vec![0u8; len];
    p.read(8, &mut data).ok()?;
    Some(data)
}

fn save(label: &str, bytes: &[u8]) -> Result<()> {
    let mut p = partition(label).ok_or_else(|| anyhow!("partition {label} missing (flash with partitions.csv)"))?;
    let total = bytes.len() + 8;
    if total > p.size() {
        bail!("{} bytes do not fit into partition {label}", bytes.len());
    }
    let mut head = [0u8; 8];
    head[..4].copy_from_slice(MAGIC);
    head[4..].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    p.erase(0, total.div_ceil(SECTOR) * SECTOR)?;
    // data first, header last: a power cut leaves no valid-looking half record
    p.write(8, bytes)?;
    p.write(0, &head)?;
    Ok(())
}
