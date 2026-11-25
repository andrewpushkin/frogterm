//! Buzzer (GPIO45, LEDC), status LED (GPIO6, active low), battery (ADC1 GPIO1,
//! enabled by GPIO21, divider x2), and the SD card kept off the panel's SPI bus.

use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{Output, OutputPin, PinDriver};
use esp_idf_svc::sys;
use frogterm_core::beep::Tone;

const BUZZER_GPIO: i32 = 45;

pub struct Board<'d> {
    led: PinDriver<'d, Output>,
    battery_enable: PinDriver<'d, Output>,
    _sd_power: PinDriver<'d, Output>,
    _sd_cs: PinDriver<'d, Output>,
}

impl<'d> Board<'d> {
    pub fn new(
        led: impl OutputPin + 'd,
        battery_enable: impl OutputPin + 'd,
        sd_power: impl OutputPin + 'd,
        sd_cs: impl OutputPin + 'd,
    ) -> Result<Self> {
        let mut sd_power = PinDriver::output(sd_power)?;
        sd_power.set_low()?;
        let mut sd_cs = PinDriver::output(sd_cs)?;
        sd_cs.set_high()?;
        let mut led = PinDriver::output(led)?;
        led.set_high()?; // off
        Ok(Self { led, battery_enable: PinDriver::output(battery_enable)?, _sd_power: sd_power, _sd_cs: sd_cs })
    }

    pub fn led(&mut self, on: bool) {
        let _ = if on { self.led.set_low() } else { self.led.set_high() };
    }

    pub fn beep(&mut self, tones: &[Tone]) {
        play(tones);
    }

    /// Battery voltage in mV (average of 8 calibrated readings), `None` on ADC errors.
    pub fn battery_mv(&mut self) -> Option<u32> {
        self.battery_enable.set_high().ok()?;
        FreeRtos::delay_ms(10);
        let mv = read_adc1_ch0_mv();
        let _ = self.battery_enable.set_low();
        mv.map(|v| v * 2)
    }
}

/// Plays `tones` without needing the `Board` (e.g. from the button thread).
pub fn play(tones: &[Tone]) {
    for t in tones {
        if t.hz > 0 {
            tone_on(t.hz);
        }
        FreeRtos::delay_ms(t.ms);
        tone_off();
    }
}

fn tone_on(hz: u32) {
    unsafe {
        let timer = sys::ledc_timer_config_t {
            speed_mode: sys::ledc_mode_t_LEDC_LOW_SPEED_MODE,
            duty_resolution: sys::ledc_timer_bit_t_LEDC_TIMER_10_BIT,
            timer_num: sys::ledc_timer_t_LEDC_TIMER_0,
            freq_hz: hz,
            clk_cfg: sys::soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK,
            ..Default::default()
        };
        sys::ledc_timer_config(&timer);
        let channel = sys::ledc_channel_config_t {
            gpio_num: BUZZER_GPIO,
            speed_mode: sys::ledc_mode_t_LEDC_LOW_SPEED_MODE,
            channel: sys::ledc_channel_t_LEDC_CHANNEL_0,
            timer_sel: sys::ledc_timer_t_LEDC_TIMER_0,
            duty: 512, // 50 % of 10 bits
            hpoint: 0,
            ..Default::default()
        };
        sys::ledc_channel_config(&channel);
    }
}

fn tone_off() {
    unsafe {
        sys::ledc_stop(sys::ledc_mode_t_LEDC_LOW_SPEED_MODE, sys::ledc_channel_t_LEDC_CHANNEL_0, 0);
    }
}

fn read_adc1_ch0_mv() -> Option<u32> {
    unsafe {
        let mut unit: sys::adc_oneshot_unit_handle_t = std::ptr::null_mut();
        let unit_cfg = sys::adc_oneshot_unit_init_cfg_t { unit_id: sys::adc_unit_t_ADC_UNIT_1, ..Default::default() };
        if sys::adc_oneshot_new_unit(&unit_cfg, &mut unit) != sys::ESP_OK {
            return None;
        }
        let chan_cfg = sys::adc_oneshot_chan_cfg_t {
            atten: sys::adc_atten_t_ADC_ATTEN_DB_12,
            bitwidth: sys::adc_bitwidth_t_ADC_BITWIDTH_DEFAULT,
        };
        sys::adc_oneshot_config_channel(unit, sys::adc_channel_t_ADC_CHANNEL_0, &chan_cfg);
        let mut cali: sys::adc_cali_handle_t = std::ptr::null_mut();
        let cali_cfg = sys::adc_cali_curve_fitting_config_t {
            unit_id: sys::adc_unit_t_ADC_UNIT_1,
            chan: sys::adc_channel_t_ADC_CHANNEL_0,
            atten: sys::adc_atten_t_ADC_ATTEN_DB_12,
            bitwidth: sys::adc_bitwidth_t_ADC_BITWIDTH_DEFAULT,
        };
        let calibrated = sys::adc_cali_create_scheme_curve_fitting(&cali_cfg, &mut cali) == sys::ESP_OK;
        let mut sum = 0u32;
        let mut n = 0u32;
        for _ in 0..8 {
            let mut raw = 0i32;
            if sys::adc_oneshot_read(unit, sys::adc_channel_t_ADC_CHANNEL_0, &mut raw) != sys::ESP_OK {
                continue;
            }
            let mut mv = 0i32;
            if calibrated && sys::adc_cali_raw_to_voltage(cali, raw, &mut mv) == sys::ESP_OK {
                sum += mv.max(0) as u32;
            } else {
                sum += (raw.max(0) as u32) * 3100 / 4095; // rough, uncalibrated
            }
            n += 1;
        }
        if calibrated {
            sys::adc_cali_delete_scheme_curve_fitting(cali);
        }
        sys::adc_oneshot_del_unit(unit);
        (n > 0).then(|| sum / n)
    }
}
