//! Deep sleep with timer + button wake. The three top buttons (active low):
//! green = update, the side buttons = browse the stored photos.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use esp_idf_svc::sys;
use frogterm_core::app;
pub use frogterm_core::app::Wake;
use frogterm_core::buttons::{self, Gesture, Holds, Press};

/// All buttons, in `buttons::ALL` order (green first).
pub const BUTTON_GPIOS: [i32; 3] = [buttons::ALL[0].1 as i32, buttons::ALL[1].1 as i32, buttons::ALL[2].1 as i32];

pub fn wake_cause() -> Wake {
    #[allow(non_upper_case_globals)]
    match unsafe { sys::esp_sleep_get_wakeup_cause() } {
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_TIMER => Wake::Timer,
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_EXT1 => {
            let mask = unsafe { sys::esp_sleep_get_ext1_wakeup_status() };
            buttons::press_from_pins(mask).map_or(Wake::PowerOn, Wake::Button)
        }
        _ => Wake::PowerOn,
    }
}

/// Why the chip restarted, if it was a crash (panic, watchdog, brownout)
/// rather than a power-on or a wake from deep sleep.
pub fn crash_reset() -> Option<&'static str> {
    #[allow(non_upper_case_globals)]
    match unsafe { sys::esp_reset_reason() } {
        sys::esp_reset_reason_t_ESP_RST_PANIC => Some("panic"),
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => Some("interrupt watchdog"),
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => Some("task watchdog"),
        sys::esp_reset_reason_t_ESP_RST_WDT => Some("watchdog"),
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => Some("brownout"),
        _ => None,
    }
}

fn is_down(gpio: i32) -> bool {
    unsafe {
        sys::gpio_set_direction(gpio, sys::gpio_mode_t_GPIO_MODE_INPUT);
        sys::gpio_set_pull_mode(gpio, sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY);
        sys::gpio_get_level(gpio) == 0
    }
}

/// True while any of the buttons is pressed.
pub fn button_held() -> bool {
    BUTTON_GPIOS.iter().any(|&g| is_down(g))
}

fn levels() -> [bool; 3] {
    BUTTON_GPIOS.map(is_down)
}

/// Milliseconds since boot.
pub fn ms() -> u64 {
    (unsafe { sys::esp_timer_get_time() } / 1000) as u64
}

/// What the button that woke the chip does: let go already or later, a
/// press; green or right still down at 10 s, a hold (settings mode). Every
/// button acts on release, so this waits for it.
pub fn gesture_after_wake(p: Press) -> Gesture {
    let Some(i) = buttons::ALL.iter().position(|(q, _)| *q == p) else { return Gesture::Press(p) };
    let down = levels();
    if !down[i] {
        return Gesture::Press(p);
    }
    let start = ms();
    let mut holds = Holds::new(down, start);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let now = ms();
        match holds.poll(levels(), now) {
            Some(g @ (Gesture::Press(q) | Gesture::Hold(q))) if q == p => return g,
            _ => {}
        }
        // `Holds` ends every gesture by then; this only guards the loop
        if now - start > buttons::STUCK_MS + 1000 {
            return Gesture::Press(p);
        }
    }
}

/// Settings mode is on: green goes to the settings loop, not to the busy pips.
static SETTINGS_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_settings_mode(on: bool) {
    SETTINGS_MODE.store(on, Ordering::Relaxed);
}

/// While the device is awake: every gesture gets the busy pips
/// (`app::press_while_awake`, as in the emulator), except in settings mode
/// the one that ends it, which comes out of the returned receiver. A button
/// still down now (the one that woke the device) counts from its next press.
pub fn spawn_gesture_listener() -> Receiver<Gesture> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("buttons".into()).stack_size(4096).spawn(move || {
        let mut holds = Holds::ignoring(levels());
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let Some(g) = holds.poll(levels(), ms()) else { continue };
            if SETTINGS_MODE.load(Ordering::Relaxed) && app::ends_settings(g) {
                let _ = tx.send(g);
            } else {
                let (Gesture::Press(p) | Gesture::Hold(p)) = g;
                crate::board::play(app::press_while_awake(p));
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("button listener: {e}");
    }
    rx
}

/// Sleep `secs`; the buttons also wake the device if `arm_buttons`.
pub fn deep_sleep(secs: u64, arm_buttons: bool) -> ! {
    log::info!("deep sleep for {secs} s (button wake {})", if arm_buttons { "on" } else { "off" });
    crate::board::hold_led_for_sleep();
    unsafe {
        sys::esp_sleep_enable_timer_wakeup(secs.max(1) * 1_000_000);
        if arm_buttons {
            let mut mask = 0u64;
            for &g in &BUTTON_GPIOS {
                sys::rtc_gpio_pullup_en(g);
                sys::rtc_gpio_pulldown_dis(g);
                mask |= 1 << g;
            }
            sys::esp_sleep_enable_ext1_wakeup(mask, sys::esp_sleep_ext1_wakeup_mode_t_ESP_EXT1_WAKEUP_ANY_LOW);
        }
        sys::esp_deep_sleep_start()
    }
}
