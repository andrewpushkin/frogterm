//! Sensirion SHT4x temperature/humidity sensor (reTerminal I2C address 0x44).
//! Command 0xFD = measure with high precision; wait 10 ms; read 6 bytes
//! [T msb, T lsb, CRC, RH msb, RH lsb, CRC].

pub const ADDR: u8 = 0x44;
pub const MEASURE_HIGH_PRECISION: u8 = 0xFD;
pub const MEASURE_MS: u32 = 10;

/// CRC-8 of Sensirion sensors (polynomial 0x31, init 0xFF).
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0xFFu8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x31 } else { crc << 1 };
        }
    }
    crc
}

/// (temperature °C, relative humidity %) or `None` if a CRC is wrong.
pub fn decode(bytes: [u8; 6]) -> Option<(f32, f32)> {
    if crc8(&bytes[0..2]) != bytes[2] || crc8(&bytes[3..5]) != bytes[5] {
        return None;
    }
    let raw = |hi: u8, lo: u8| f32::from(u16::from_be_bytes([hi, lo])) / 65535.0;
    let t = -45.0 + 175.0 * raw(bytes[0], bytes[1]);
    let rh = (-6.0 + 125.0 * raw(bytes[3], bytes[4])).clamp(0.0, 100.0);
    Some((t, rh))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_datasheet_example() {
        assert_eq!(crc8(&[0xBE, 0xEF]), 0x92);
    }

    #[test]
    fn decodes_temperature_and_humidity() {
        // raw 0x6666 = 0.4 of full scale: T = -45 + 175 * 0.4 = 25 °C, RH = -6 + 125 * 0.4 = 44 %
        let b = [0x66, 0x66, crc8(&[0x66, 0x66]), 0x66, 0x66, crc8(&[0x66, 0x66])];
        let (t, rh) = decode(b).unwrap();
        assert!((t - 25.0).abs() < 0.01, "{t}");
        assert!((rh - 44.0).abs() < 0.01, "{rh}");
    }

    #[test]
    fn humidity_is_clamped_to_0_100() {
        let b = [0x66, 0x66, crc8(&[0x66, 0x66]), 0xFF, 0xFF, crc8(&[0xFF, 0xFF])];
        assert_eq!(decode(b).unwrap().1, 100.0);
    }

    #[test]
    fn corrupted_reading_is_rejected() {
        let b = [0x66, 0x66, 0x00, 0x66, 0x66, crc8(&[0x66, 0x66])];
        assert_eq!(decode(b), None);
    }
}
