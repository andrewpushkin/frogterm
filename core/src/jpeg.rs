//! Baseline JPEG to RGB888, pure Rust (runs on the ESP32; output goes to PSRAM).

use anyhow::{anyhow, bail, Context, Result};
use jpeg_decoder::{Decoder, PixelFormat};

/// RGB888 bytes of a `w`×`h` JPEG; grayscale is expanded to RGB.
/// Google always sends the requested size, so any other size is an error.
pub fn decode_rgb(bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut dec = Decoder::new(bytes);
    let px = dec.decode().context("decode JPEG")?;
    let info = dec.info().ok_or_else(|| anyhow!("JPEG has no header"))?;
    if (u32::from(info.width), u32::from(info.height)) != (w, h) {
        bail!("JPEG is {}x{}, expected {w}x{h}", info.width, info.height);
    }
    Ok(match info.pixel_format {
        PixelFormat::RGB24 => px,
        PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(),
        PixelFormat::L16 => px.chunks(2).flat_map(|v| [v[0], v[0], v[0]]).collect(),
        PixelFormat::CMYK32 => px
            .chunks(4)
            .flat_map(|c| {
                let k = u16::from(c[3]);
                let f = |v: u8| ((u16::from(v) * k) / 255) as u8; // Adobe CMYK is stored inverted
                [f(c[0]), f(c[1]), f(c[2])]
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::jpeg::JpegEncoder, ExtendedColorType};

    fn jpeg(w: u32, h: u32, px: &[u8], color: ExtendedColorType) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 95).encode(px, w, h, color).unwrap();
        out
    }

    #[test]
    fn decodes_a_colour_jpeg() {
        let px: Vec<u8> = (0..16 * 8).flat_map(|_| [200u8, 40, 40]).collect();
        let rgb = decode_rgb(&jpeg(16, 8, &px, ExtendedColorType::Rgb8), 16, 8).unwrap();
        assert_eq!(rgb.len(), 16 * 8 * 3);
        for p in rgb.chunks(3) {
            assert!(p[0].abs_diff(200) <= 6 && p[1].abs_diff(40) <= 6 && p[2].abs_diff(40) <= 6, "{p:?}");
        }
    }

    #[test]
    fn grayscale_becomes_rgb() {
        let px = vec![128u8; 8 * 8];
        let rgb = decode_rgb(&jpeg(8, 8, &px, ExtendedColorType::L8), 8, 8).unwrap();
        assert_eq!(rgb.len(), 8 * 8 * 3);
        assert!(rgb.chunks(3).all(|p| p[0] == p[1] && p[1] == p[2] && p[0].abs_diff(128) <= 4));
    }

    #[test]
    fn wrong_size_is_an_error() {
        let px = vec![0u8; 16 * 8 * 3];
        assert!(decode_rgb(&jpeg(16, 8, &px, ExtendedColorType::Rgb8), 800, 480).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode_rgb(b"not a jpeg", 1, 1).is_err());
    }
}
