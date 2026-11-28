//! Button presses typed on the serial console (UART0, the reTerminal's
//! USB-serial port), for testing: `green`, `next`, `previous`, `hold green`,
//! `hold next`, `version` (see `buttons::parse_command`).
//!
//! While the device is awake a command acts as the real button would (its
//! level is held down for a moment, or 10.5 s for a hold). The chip hears
//! nothing in deep sleep, so a test resets it (power-on) and types the
//! command within `BOOT_WINDOW` of the start: it then counts as the button
//! that woke the device (`tools/serial.py press`).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Mutex;
use std::time::Duration;

use esp_idf_svc::sys;
use frogterm_core::buttons::{self, parse_command, Command, Gesture};

/// After a power-on, a command typed this soon is the waking button.
pub const BOOT_WINDOW: Duration = Duration::from_secs(3);
/// How long a typed press holds the button down.
const PRESS_MS: u32 = 150;
/// A typed hold: past the 10-s mark.
const HOLD_MS: u32 = buttons::HOLD_MS as u32 + 500;
const UART: sys::uart_port_t = 0;

/// Until when (milliseconds since boot, wrapping) each button (in
/// `buttons::ALL` order) is held down by a command.
static DOWN_UNTIL: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
/// Commands go to `first_command` instead of the buttons.
static IN_BOOT_WINDOW: AtomicBool = AtomicBool::new(false);
static FIRST: Mutex<Option<SyncSender<Gesture>>> = Mutex::new(None);

fn now_ms() -> u32 {
    crate::sleep::ms() as u32
}

/// The buttons a command holds down now.
pub fn simulated() -> [bool; 3] {
    let now = now_ms();
    DOWN_UNTIL.each_ref().map(|t| (t.load(Ordering::Relaxed).wrapping_sub(now) as i32) > 0)
}

/// Starts reading the console. With `boot_window`, the returned receiver
/// gets a gesture typed within `BOOT_WINDOW` (see `first_command`).
pub fn start(boot_window: bool) -> Option<Receiver<Gesture>> {
    // SAFETY: plain ESP-IDF calls on UART0, the console; the log then goes
    // through the same driver
    let installed = unsafe {
        sys::uart_driver_install(UART, 512, 0, 0, std::ptr::null_mut(), 0) == sys::ESP_OK && {
            sys::uart_vfs_dev_use_driver(UART as i32);
            true
        }
    };
    if !installed {
        log::warn!("serial console: no UART driver, commands off");
        return None;
    }
    let rx = boot_window.then(|| {
        let (tx, rx) = mpsc::sync_channel(1);
        *FIRST.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        IN_BOOT_WINDOW.store(true, Ordering::Relaxed);
        rx
    });
    let spawned = std::thread::Builder::new().name("serial".into()).stack_size(4096).spawn(read_lines);
    if let Err(e) = spawned {
        log::warn!("serial console: {e}");
        return None;
    }
    rx
}

/// The gesture typed within `BOOT_WINDOW`, if any; afterwards commands act
/// on the buttons.
pub fn first_command(rx: Option<Receiver<Gesture>>) -> Option<Gesture> {
    let rx = rx?;
    log::info!("serial: type a button within {} s (green, next, previous, hold green, hold next)", BOOT_WINDOW.as_secs());
    let got = rx.recv_timeout(BOOT_WINDOW).ok();
    IN_BOOT_WINDOW.store(false, Ordering::Relaxed);
    FIRST.lock().unwrap_or_else(|e| e.into_inner()).take();
    got
}

fn read_lines() {
    let mut line = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        // SAFETY: reads into our buffer, waiting at most 100 ms
        let n = unsafe { sys::uart_read_bytes(UART, buf.as_mut_ptr().cast(), buf.len() as u32, 100) };
        for &b in buf.iter().take(n.max(0) as usize) {
            match b {
                b'\n' | b'\r' => {
                    if !line.is_empty() {
                        act(&String::from_utf8_lossy(&line));
                        line.clear();
                    }
                }
                _ if line.len() < 64 => line.push(b),
                _ => {}
            }
        }
    }
}

fn act(line: &str) {
    match parse_command(line) {
        Some(Command::Version) => log::info!("serial: frogterm {}", crate::ota::version()),
        Some(Command::Gesture(g)) if IN_BOOT_WINDOW.load(Ordering::Relaxed) => {
            if let Some(tx) = FIRST.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                let _ = tx.try_send(g);
            }
        }
        Some(Command::Gesture(g)) => {
            let (Gesture::Press(p) | Gesture::Hold(p)) = g;
            let ms = if matches!(g, Gesture::Hold(_)) { HOLD_MS } else { PRESS_MS };
            if let Some(i) = buttons::ALL.iter().position(|(q, _)| *q == p) {
                log::info!("serial: {g:?}");
                DOWN_UNTIL[i].store(now_ms().wrapping_add(ms), Ordering::Relaxed);
            }
        }
        None => log::warn!("serial: unknown command {line:?} (green, next, previous, hold green, hold next, version)"),
    }
}
