//! Deep sleep with timer + green-button (KEY0, GPIO3, active low) wake.

use esp_idf_svc::sys;

pub const BUTTON_GPIO: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Timer,
    Button,
    PowerOn,
}

pub fn wake_cause() -> Wake {
    #[allow(non_upper_case_globals)]
    match unsafe { sys::esp_sleep_get_wakeup_cause() } {
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_TIMER => Wake::Timer,
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_EXT1 => Wake::Button,
        _ => Wake::PowerOn,
    }
}

/// True while the green button is pressed.
pub fn button_held() -> bool {
    unsafe {
        sys::gpio_set_direction(BUTTON_GPIO, sys::gpio_mode_t_GPIO_MODE_INPUT);
        sys::gpio_set_pull_mode(BUTTON_GPIO, sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY);
        sys::gpio_get_level(BUTTON_GPIO) == 0
    }
}

/// While the device is awake, every new press of the green button gets a
/// short chirp and is otherwise ignored (the running update just goes on).
pub fn spawn_press_listener() {
    let spawned = std::thread::Builder::new().name("button".into()).stack_size(4096).spawn(|| {
        let mut was_down = button_held(); // the press that woke us is not a new one
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let down = button_held();
            if down && !was_down {
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

/// Sleep `secs`; the button also wakes the device if `arm_button`.
pub fn deep_sleep(secs: u64, arm_button: bool) -> ! {
    log::info!("deep sleep for {secs} s (button wake {})", if arm_button { "on" } else { "off" });
    unsafe {
        sys::esp_sleep_enable_timer_wakeup(secs.max(1) * 1_000_000);
        if arm_button {
            sys::rtc_gpio_pullup_en(BUTTON_GPIO);
            sys::rtc_gpio_pulldown_dis(BUTTON_GPIO);
            sys::esp_sleep_enable_ext1_wakeup(1u64 << BUTTON_GPIO, sys::esp_sleep_ext1_wakeup_mode_t_ESP_EXT1_WAKEUP_ANY_LOW);
        }
        sys::esp_deep_sleep_start()
    }
}
