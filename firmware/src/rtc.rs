//! I2C0 (SDA 19, SCL 20): PCF8563 real-time clock (kept in UTC) and the
//! SHT4x room temperature/humidity sensor.

use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{InputPin, OutputPin};
use esp_idf_svc::hal::i2c::{I2c, I2cConfig, I2cDriver};
use esp_idf_svc::hal::units::Hertz;
use frogterm_core::{pcf8563, sht4x};

const ADDR: u8 = 0x51;
/// I2C timeout in FreeRTOS ticks (1 tick = 1 ms).
const TIMEOUT: u32 = 100;

pub struct Rtc<'d> {
    i2c: I2cDriver<'d>,
}

impl<'d> Rtc<'d> {
    pub fn new(i2c: impl I2c + 'd, sda: impl InputPin + OutputPin + 'd, scl: impl InputPin + OutputPin + 'd) -> Result<Self> {
        let cfg = I2cConfig::new().baudrate(Hertz(100_000));
        Ok(Self { i2c: I2cDriver::new(i2c, sda, scl, &cfg)? })
    }

    /// Unix time, or `None` if the clock lost power since it was last set.
    pub fn read(&mut self) -> Result<Option<i64>> {
        let mut regs = [0u8; 7];
        self.i2c.write_read(ADDR, &[0x02], &mut regs, TIMEOUT)?;
        Ok(pcf8563::decode(regs))
    }

    /// Room (temperature °C, humidity %) from the SHT4x on the same bus.
    pub fn room_climate(&mut self) -> Result<Option<(f32, f32)>> {
        self.i2c.write(sht4x::ADDR, &[sht4x::MEASURE_HIGH_PRECISION], TIMEOUT)?;
        FreeRtos::delay_ms(sht4x::MEASURE_MS);
        let mut b = [0u8; 6];
        self.i2c.read(sht4x::ADDR, &mut b, TIMEOUT)?;
        Ok(sht4x::decode(b))
    }

    pub fn write(&mut self, unix: i64) -> Result<()> {
        let r = pcf8563::encode(unix);
        let mut buf = [0u8; 8];
        buf[0] = 0x02;
        buf[1..].copy_from_slice(&r);
        self.i2c.write(ADDR, &buf, TIMEOUT)?;
        Ok(())
    }
}
