//! ED2208-GCA (reTerminal E1002, 7.3" Spectra 6) command protocol.
//!
//! Values from Zephyr's `drivers/display/display_ed2208_gca.c` (Apache-2.0),
//! which match Seeed_GFX `ED2208_Init.h`. Hardware access lives in the firmware.

/// Init sequence: (command, data), sent in order after a reset.
/// The last entry is "power on"; wait for BUSY (≤ `POWER_TIMEOUT_MS`) after it.
pub const INIT: &[(u8, &[u8])] = &[
    (0xAA, &[0x49, 0x55, 0x20, 0x08, 0x09, 0x18]), // CMDH
    (0x01, &[0x3F, 0x00, 0x32, 0x2A, 0x0E, 0x2A]), // power setting
    (0x00, &[0x5F, 0x69]),                         // panel setting
    (0x03, &[0x00, 0x54, 0x00, 0x44]),             // power off sequence
    (0x05, &[0x40, 0x1F, 0x1F, 0x2C]),             // booster soft start 1
    (0x06, &[0x6F, 0x1F, 0x16, 0x25]),             // booster soft start 2
    (0x08, &[0x6F, 0x1F, 0x1F, 0x22]),             // booster soft start 3
    (0x13, &[0x00, 0x04]),                         // IPC
    (0x30, &[0x02]),                               // PLL
    (0x41, &[0x00]),                               // temperature sensor
    (0x50, &[0x3F]),                               // VCOM / data interval
    (0x60, &[0x02, 0x00]),                         // TCON
    (0x61, &[0x03, 0x20, 0x01, 0xE0]),             // resolution 800 x 480
    (0x82, &[0x1E]),                               // VCOM DC
    (0x84, &[0x01]),                               // T_VDCS
    (0x86, &[0x00]),                               // AGID
    (0xE3, &[0x2F]),                               // power saving
    (0xE0, &[0x00]),                               // CCSET
    (0xE6, &[0x00]),                               // TSSET
    (0x04, &[]),                                   // power on
];

/// Start of pixel data (followed by 192 000 bytes, left pixel = high nibble).
pub const DATA_START: u8 = 0x10;
/// Display refresh; wait for BUSY ≤ `REFRESH_TIMEOUT_MS`.
pub const REFRESH: (u8, &[u8]) = (0x12, &[0x00]);
/// Power off; wait for BUSY ≤ `POWER_TIMEOUT_MS`.
pub const POWER_OFF: (u8, &[u8]) = (0x02, &[0x00]);
/// Deep sleep (needs a reset to wake).
pub const DEEP_SLEEP: (u8, &[u8]) = (0x07, &[0xA5]);

pub const RESET_LOW_MS: u32 = 20;
pub const RESET_WAIT_MS: u32 = 10;
pub const REFRESH_TIMEOUT_MS: u32 = 40_000;
pub const POWER_TIMEOUT_MS: u32 = 5_000;
pub const SPI_HZ: u32 = 4_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_is_800_by_480() {
        let tres = INIT.iter().find(|(c, _)| *c == 0x61).expect("TRES present");
        assert_eq!(tres.1, &[0x03, 0x20, 0x01, 0xE0]);
    }

    #[test]
    fn init_ends_with_power_on() {
        assert_eq!(INIT.last(), Some(&(0x04u8, &[][..])));
    }

    #[test]
    fn commands_match_the_reference_order() {
        let cmds: Vec<u8> = INIT.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            cmds,
            [0xAA, 0x01, 0x00, 0x03, 0x05, 0x06, 0x08, 0x13, 0x30, 0x41, 0x50, 0x60, 0x61, 0x82, 0x84, 0x86, 0xE3, 0xE0, 0xE6, 0x04]
        );
    }
}
