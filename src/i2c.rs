//! The I²C bus on GPIO18/19, which the DS3231 has to itself on `0x68`.
//!
//! The RTC owns it outright: with one device there is nothing to share, so it
//! takes the async driver directly rather than a shared-bus handle.

use esp_hal::gpio::interconnect::{PeripheralInput, PeripheralOutput};
use esp_hal::i2c::master::{Config, ConfigError, I2c};
use esp_hal::time::Rate;
use esp_hal::{Async, peripherals};

/// 400 kHz. The DS3231 handles it, and it is four times less time on the wire
/// than the 100 kHz default.
const BUS_HZ: u32 = 400;

/// The bus, in async mode, for [`crate::rtc::Rtc`].
pub type Bus = I2c<'static, Async>;

/// Brings up the peripheral.
pub fn bus(
    i2c: peripherals::I2C0<'static>,
    sda: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    scl: impl PeripheralInput<'static> + PeripheralOutput<'static>,
) -> Result<Bus, ConfigError> {
    Ok(I2c::new(
        i2c,
        Config::default().with_frequency(Rate::from_khz(BUS_HZ)),
    )?
    .with_sda(sda)
    .with_scl(scl)
    .into_async())
}
