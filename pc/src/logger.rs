//! Log lines of the shared device code (`log` crate) on the console, with
//! the local time; the last one is also shown in the emulator window.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LAST: Mutex<String> = Mutex::new(String::new());

struct Console;

impl log::Log for Console {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let secs = frogterm_core::tz::to_local(now).1;
        let level = match r.level() {
            log::Level::Error => "ERROR ",
            log::Level::Warn => "warn  ",
            _ => "",
        };
        let line = format!("{level}{}", r.args());
        println!("{:02}:{:02}:{:02} {line}", secs / 3600, secs % 3600 / 60, secs % 60);
        if let Ok(mut last) = LAST.lock() {
            *last = line;
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    static LOGGER: Console = Console;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// The last line logged.
pub fn last() -> String {
    LAST.lock().map(|l| l.clone()).unwrap_or_default()
}
