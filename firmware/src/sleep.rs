//! Deep sleep with timer + button wake. The three top buttons (active low):
//! green = update, the side buttons = browse the stored photos.

use esp_idf_svc::sys;
use frogterm_core::buttons::{self, Press};

/// All buttons, green first.
pub const BUTTON_GPIOS: [i32; 3] =
    [buttons::GREEN_GPIO as i32, buttons::NEXT_GPIO as i32, buttons::PREVIOUS_GPIO as i32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Timer,
    Button(Press),
    PowerOn,
}

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

/// While the device is awake, every new press of any button gets a short
/// chirp and is otherwise ignored (the running update just goes on).
pub fn spawn_press_listener() {
    let spawned = std::thread::Builder::new().name("buttons".into()).stack_size(4096).spawn(|| {
        // presses already down (e.g. the one that woke us) are not new ones
        let mut was_down = BUTTON_GPIOS.map(is_down);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let down = BUTTON_GPIOS.map(is_down);
            if down.iter().zip(&was_down).any(|(&d, &w)| d && !w) {
                log::info!("button pressed during the update: ignored");
                crate::board::play(frogterm_core::beep::PRESS);
            }
            was_down = down;
        }
    });
    if let Err(e) = spawned {
        log::warn!("button listener: {e}");
    }
}

/// Sleep `secs`; the buttons also wake the device if `arm_buttons`.
pub fn deep_sleep(secs: u64, arm_buttons: bool) -> ! {
    log::info!("deep sleep for {secs} s (button wake {})", if arm_buttons { "on" } else { "off" });
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
