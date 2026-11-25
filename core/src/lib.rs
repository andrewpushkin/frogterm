//! Hardware-independent logic shared by the reTerminal firmware and the PC tools.
//! Keep dependencies small and embedded-friendly: this code runs on the ESP32-S3.

pub mod album;
pub mod battery;
pub mod beep;
pub mod calendar;
pub mod date;
pub mod dither;
pub mod epd;
pub mod framebuf;
pub mod info;
pub mod jpeg;
pub mod layout;
pub mod moon;
pub mod pcf8563;
pub mod photo_list;
pub mod schedule;
pub mod screen;
pub mod sht4x;
pub mod text;
pub mod tz;
pub mod url;
pub mod weather;
