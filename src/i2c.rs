//! The one I²C bus, shared by the panel and the RTC.
//!
//! GPIO18/19 carry both: the SSD1315 on `0x3C` and the DS3231 on `0x68`. Each
//! gets an [`Device`] — `embassy-embedded-hal`'s shared-bus handle — which
//! locks the bus for one transaction at a time, so a panel flush and a clock
//! read cannot interleave mid-transfer.
//!
//! ⚠️ **Two modules usually means two sets of pull-ups** in parallel on `SDA`
//! and `SCL`. At 400 kHz that is normally fine; it is the first thing to check
//! if the panel starts misbehaving only after the RTC goes on.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::gpio::interconnect::{PeripheralInput, PeripheralOutput};
use esp_hal::i2c::master::{Config, ConfigError, I2c};
use esp_hal::time::Rate;
use esp_hal::{Async, peripherals};

/// 400 kHz. Both parts handle it, and it is four times less time on the wire
/// than the 100 kHz default — which matters because every millisecond here is
/// a millisecond the bus is locked against the other device.
const BUS_HZ: u32 = 400;

/// The bus itself, behind the lock both devices share.
pub type Bus = Mutex<CriticalSectionRawMutex, I2c<'static, Async>>;

/// One device's handle on [`Bus`].
pub type Device<'b> = I2cDevice<'b, CriticalSectionRawMutex, I2c<'static, Async>>;

/// Brings up the peripheral. Put the result in a static and hand out
/// [`Device`]s from it.
pub fn bus(
    i2c: peripherals::I2C0<'static>,
    sda: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    scl: impl PeripheralInput<'static> + PeripheralOutput<'static>,
) -> Result<Bus, ConfigError> {
    let i2c = I2c::new(
        i2c,
        Config::default().with_frequency(Rate::from_khz(BUS_HZ)),
    )?
    .with_sda(sda)
    .with_scl(scl)
    .into_async();
    Ok(Mutex::new(i2c))
}
