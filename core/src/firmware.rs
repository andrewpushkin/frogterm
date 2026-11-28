//! Checks a firmware file uploaded on the settings page before (and while)
//! the device installs it: an ESP32-S3 app image (`firmware.bin`, not the
//! full flash image) that carries frogterm's version tag.

use anyhow::{bail, Result};

/// The largest firmware the update slots hold.
pub const MAX_IMAGE: usize = 0x30_0000;
/// Every frogterm firmware contains `<frogterm-version:N>`.
pub const VERSION_PREFIX: &[u8] = b"<frogterm-version:";
/// Where CI publishes each release (GitHub Pages): `version.txt` (the
/// release number) and `firmware.bin`.
pub const UPDATE_SITE: &str = "https://andrewpushkin.github.io/frogterm/";

/// Should a firmware of version `running` install the published `latest`?
/// Only a numbered firmware updates itself, and only to a higher number;
/// one without a number ("dev", a CI build of a branch) stays as it is.
pub fn newer(running: &str, latest: &str) -> bool {
    match (running.trim().parse::<u32>(), latest.trim().parse::<u32>()) {
        (Ok(r), Ok(l)) => l > r,
        _ => false,
    }
}

/// Bytes `check_header` needs.
pub const HEADER_LEN: usize = 36;

const IMAGE_MAGIC: u8 = 0xE9;
const ESP32S3_CHIP_ID: u16 = 9;
/// `esp_app_desc_t` at the start of an app's first segment.
const APP_DESC_MAGIC: u32 = 0xABCD_5432;

/// The start of the file: an ESP32-S3 app image?
pub fn check_header(head: &[u8]) -> Result<()> {
    if head.len() < HEADER_LEN {
        bail!("this file is too short to be firmware");
    }
    if head[0] != IMAGE_MAGIC {
        bail!("this is not ESP32 firmware: upload firmware.bin from a frogterm release");
    }
    if u16::from_le_bytes([head[12], head[13]]) != ESP32S3_CHIP_ID {
        bail!("this firmware is for another chip (the reTerminal E1002 is an ESP32-S3)");
    }
    if u32::from_le_bytes([head[32], head[33], head[34], head[35]]) != APP_DESC_MAGIC {
        bail!("this is a full flash image (bootloader first), e.g. firmware-full.bin: upload firmware.bin");
    }
    Ok(())
}

/// Finds the version tag in a file that comes in pieces.
#[derive(Default)]
pub struct TagScanner {
    /// The end of the last piece, for a tag split between two pieces.
    tail: Vec<u8>,
    version: Option<String>,
}

/// A version is short: digits, letters, `-`.
const MAX_VERSION: usize = 40;

impl TagScanner {
    pub fn new() -> TagScanner {
        TagScanner::default()
    }

    pub fn feed(&mut self, piece: &[u8]) {
        if self.version.is_some() {
            return;
        }
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(piece);
        let mut from = 0;
        while let Some(i) = find(&buf[from..], VERSION_PREFIX).map(|i| from + i) {
            let rest = &buf[i + VERSION_PREFIX.len()..];
            match rest.iter().position(|&b| b == b'>') {
                Some(end) if end > 0 && end <= MAX_VERSION && rest[..end].iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-') => {
                    self.version = Some(String::from_utf8_lossy(&rest[..end]).into_owned());
                    return;
                }
                _ => from = i + 1,
            }
        }
        // a tag cut off at the end comes whole with the next piece
        let keep = (VERSION_PREFIX.len() + MAX_VERSION + 1).min(buf.len());
        self.tail = buf[buf.len() - keep..].to_vec();
    }

    /// The version, once the tag was seen.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

/// A whole file: frogterm firmware for this device? Returns its version.
pub fn check_image(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_IMAGE {
        bail!("this file is larger than the {} MB a firmware may have", MAX_IMAGE / (1024 * 1024));
    }
    check_header(bytes)?;
    let mut scanner = TagScanner::new();
    scanner.feed(bytes);
    match scanner.version() {
        Some(v) => Ok(v.to_string()),
        None => bail!("this is not frogterm firmware"),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app image as the build makes it: header, app description, the tag.
    fn app(version: &str) -> Vec<u8> {
        let mut b = vec![0u8; 4096];
        b[0] = 0xE9;
        b[1] = 7;
        b[12..14].copy_from_slice(&9u16.to_le_bytes());
        b[32..36].copy_from_slice(&0xABCD_5432u32.to_le_bytes());
        let tag = format!("<frogterm-version:{version}>");
        b[3000..3000 + tag.len()].copy_from_slice(tag.as_bytes());
        b
    }

    #[test]
    fn only_a_release_updates_itself_to_a_newer_one() {
        assert!(newer("85", "86"));
        assert!(newer("85", " 100\n"), "version.txt may end with a newline");
        assert!(!newer("85", "85"));
        assert!(!newer("86", "85"), "never back");
        assert!(!newer("dev", "86"), "no number: stays");
        assert!(!newer("dev-1a2b3c", "86"));
        assert!(!newer("85", "<html>"), "garbage is no version");
        assert!(UPDATE_SITE.starts_with("https://") && UPDATE_SITE.ends_with('/'));
    }

    #[test]
    fn a_frogterm_app_image_passes_with_its_version() {
        assert_eq!(check_image(&app("85")).unwrap(), "85");
        assert_eq!(check_image(&app("dev")).unwrap(), "dev");
    }

    #[test]
    fn the_full_flash_image_is_refused_by_name() {
        let mut b = app("85");
        b[32..36].copy_from_slice(&[0x50, 0, 0, 0]); // the bootloader comes first
        let e = check_header(&b).unwrap_err().to_string();
        assert!(e.contains("firmware-full.bin") && e.contains("firmware.bin"), "{e}");
    }

    #[test]
    fn other_files_are_refused() {
        let mut other_chip = app("85");
        other_chip[12] = 0; // ESP32
        assert!(check_header(&other_chip).unwrap_err().to_string().contains("another chip"));
        let mut jpeg = app("85");
        jpeg[0] = 0xFF;
        assert!(check_header(&jpeg).is_err());
        assert!(check_header(&[0xE9; 10]).unwrap_err().to_string().contains("too short"));
        let mut foreign = app("85");
        foreign[3000..3010].copy_from_slice(b"xxxxxxxxxx");
        assert!(check_image(&foreign).unwrap_err().to_string().contains("not frogterm"));
        assert!(check_image(&vec![0u8; MAX_IMAGE + 1]).unwrap_err().to_string().contains("larger"));
    }

    #[test]
    fn the_tag_is_found_across_pieces() {
        let b = app("1234");
        for cut in [3001, 3010, 3018, 3020, 3023] {
            let mut s = TagScanner::new();
            s.feed(&b[..cut]);
            s.feed(&b[cut..]);
            assert_eq!(s.version(), Some("1234"), "cut at {cut}");
        }
        let mut s = TagScanner::new();
        for piece in b.chunks(7) {
            s.feed(piece);
        }
        assert_eq!(s.version(), Some("1234"));
    }

    #[test]
    fn a_prefix_without_a_proper_end_is_no_tag() {
        let mut s = TagScanner::new();
        s.feed(b"<frogterm-version:85 <frogterm-version:>  <frogterm-version:a b>");
        s.feed(&[0u8; 100]);
        assert_eq!(s.version(), None);
        s.feed(b"..<frogterm-version:86>");
        assert_eq!(s.version(), Some("86"));
    }
}
