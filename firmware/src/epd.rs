//! ED2208-GCA e-paper panel on SPI2 (protocol table in `frogterm_core::epd`).

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyIOPin, Input, InputPin, Output, OutputPin, PinDriver, Pull};
use esp_idf_svc::hal::spi::config::{Config, DriverConfig};
use esp_idf_svc::hal::spi::{SpiAnyPins, SpiDeviceDriver, SpiDriver};
use esp_idf_svc::hal::units::Hertz;
use frogterm_core::epd;
use frogterm_core::framebuf::FrameBuffer;

pub struct Epd<'d> {
    spi: SpiDeviceDriver<'d, SpiDriver<'d>>,
    dc: PinDriver<'d, Output>,
    rst: PinDriver<'d, Output>,
    busy: PinDriver<'d, Input>,
}

impl<'d> Epd<'d> {
    pub fn new(
        spi: impl SpiAnyPins + 'd,
        sck: impl OutputPin + 'd,
        mosi: impl OutputPin + 'd,
        cs: impl OutputPin + 'd,
        dc: impl OutputPin + 'd,
        rst: impl OutputPin + 'd,
        busy: impl InputPin + 'd,
    ) -> Result<Self> {
        let spi = SpiDeviceDriver::new_single(
            spi,
            sck,
            mosi,
            Option::<AnyIOPin>::None,
            Some(cs),
            &DriverConfig::new(),
            &Config::new().baudrate(Hertz(epd::SPI_HZ)),
        )?;
        Ok(Self {
            spi,
            dc: PinDriver::output(dc)?,
            rst: PinDriver::output(rst)?,
            busy: PinDriver::input(busy, Pull::Up)?,
        })
    }

    /// Reset, init, send the frame, refresh, power off, deep sleep.
    pub fn show(&mut self, fb: &FrameBuffer) -> Result<()> {
        let t = Instant::now();
        self.rst.set_low()?;
        FreeRtos::delay_ms(epd::RESET_LOW_MS);
        self.rst.set_high()?;
        FreeRtos::delay_ms(epd::RESET_WAIT_MS);
        for (cmd, data) in epd::INIT {
            self.command(*cmd, data)?;
        }
        self.wait_idle(epd::POWER_TIMEOUT_MS, "power on")?;
        self.command(epd::DATA_START, &[])?;
        self.dc.set_high()?;
        for chunk in fb.as_bytes().chunks(4096) {
            self.spi.write(chunk)?;
        }
        self.command(epd::REFRESH.0, epd::REFRESH.1)?;
        self.wait_idle(epd::REFRESH_TIMEOUT_MS, "refresh")?;
        self.command(epd::POWER_OFF.0, epd::POWER_OFF.1)?;
        self.wait_idle(epd::POWER_TIMEOUT_MS, "power off")?;
        self.command(epd::DEEP_SLEEP.0, epd::DEEP_SLEEP.1)?;
        log::info!("panel updated in {:.1} s", t.elapsed().as_secs_f32());
        Ok(())
    }

    fn command(&mut self, cmd: u8, data: &[u8]) -> Result<()> {
        self.dc.set_low()?;
        self.spi.write(&[cmd])?;
        if !data.is_empty() {
            self.dc.set_high()?;
            self.spi.write(data)?;
        }
        Ok(())
    }

    /// BUSY is LOW while the panel works. The CPU light-sleeps meanwhile
    /// (the panel refreshes on its own) and wakes when BUSY goes high.
    fn wait_idle(&mut self, timeout_ms: u32, what: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        while self.busy.is_low() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("panel busy for more than {timeout_ms} ms during {what}");
            }
            light_sleep_until_busy_high(left);
        }
        Ok(())
    }

    /// Plain polling, kept for comparison/debugging.
    #[allow(dead_code)]
    fn wait_idle_polling(&mut self, timeout_ms: u32, what: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        FreeRtos::delay_ms(1);
        while self.busy.is_low() {
            if Instant::now() > deadline {
                bail!("panel busy for more than {timeout_ms} ms during {what}");
            }
            FreeRtos::delay_ms(10);
        }
        Ok(())
    }
}

/// Light sleep (~1 mA) until the panel's BUSY line (GPIO13) goes high or `max` passes.
fn light_sleep_until_busy_high(max: Duration) {
    use esp_idf_svc::sys;
    const BUSY_GPIO: i32 = 13;
    unsafe {
        // keep every pin as configured: by default light sleep isolates GPIOs,
        // which disconnects BUSY (no wake-up) and the panel/buzzer lines
        sys::esp_sleep_enable_gpio_switch(false);
        sys::gpio_wakeup_enable(BUSY_GPIO, sys::gpio_int_type_t_GPIO_INTR_HIGH_LEVEL);
        // a button press also wakes us, so the button thread can chirp
        for &b in &crate::sleep::BUTTON_GPIOS {
            sys::gpio_wakeup_enable(b, sys::gpio_int_type_t_GPIO_INTR_LOW_LEVEL);
        }
        sys::esp_sleep_enable_gpio_wakeup();
        sys::esp_sleep_enable_timer_wakeup(max.as_micros().max(1000) as u64);
        sys::esp_light_sleep_start();
        sys::gpio_wakeup_disable(BUSY_GPIO);
        for &b in &crate::sleep::BUTTON_GPIOS {
            sys::gpio_wakeup_disable(b);
        }
        sys::esp_sleep_disable_wakeup_source(sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_ALL);
    }
    if crate::sleep::button_held() {
        // woken by the button: stay awake briefly so the button thread can chirp
        FreeRtos::delay_ms(50);
    }
}
