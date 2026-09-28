//! The DS3231 itself, over the shared I²C bus. Bytes in and out.
//!
//! [`crate::ds3231`] decides what the registers mean and is host-tested; this
//! only moves them. Same split as `oled.rs` and `display.rs`.

use embedded_hal_async::i2c::I2c as _;

use crate::ds3231::{
    self, ADDRESS, CONTROL_REG, OFFSET_REG, REGISTERS, Reading, TIME_REG, cleared_status,
    encode_offset, encode_time, running_control,
};
use crate::i2c::Device;
use crate::schedule::Wall;

#[derive(Debug)]
pub enum Error {
    /// Nothing answered on `0x68`, or a transfer failed part-way.
    Bus,
    /// A year the chip cannot hold (outside 2000–2099).
    Unrepresentable,
}

pub struct Rtc<'d> {
    bus: Device<'d>,
}

impl<'d> Rtc<'d> {
    pub fn new(bus: Device<'d>) -> Self {
        Self { bus }
    }

    /// Every register this firmware cares about, in one burst.
    pub async fn read(&mut self) -> Result<Reading, Error> {
        let mut regs = [0u8; REGISTERS];
        self.bus
            .write_read(ADDRESS, &[TIME_REG], &mut regs)
            .await
            .map_err(|_| Error::Bus)?;
        Ok(ds3231::decode(&regs))
    }

    /// Sets the time and the offset it is in, and clears `OSF` and `EOSC` so
    /// the chip both admits to being set and keeps running on its coin cell.
    ///
    /// Time first, flags second: clearing `OSF` is the claim that the time is
    /// good, so it must not land before the time does.
    pub async fn set(&mut self, wall: Wall) -> Result<(), Error> {
        let time = encode_time(wall).ok_or(Error::Unrepresentable)?;

        let mut write = [0u8; 8];
        write[0] = TIME_REG;
        write[1..].copy_from_slice(&time);
        self.bus
            .write(ADDRESS, &write)
            .await
            .map_err(|_| Error::Bus)?;

        let [a, b, c] = encode_offset(wall.offset_minutes);
        self.bus
            .write(ADDRESS, &[OFFSET_REG, a, b, c])
            .await
            .map_err(|_| Error::Bus)?;

        let mut flags = [0u8; 2];
        self.bus
            .write_read(ADDRESS, &[CONTROL_REG], &mut flags)
            .await
            .map_err(|_| Error::Bus)?;
        self.bus
            .write(
                ADDRESS,
                &[
                    CONTROL_REG,
                    running_control(flags[0]),
                    cleared_status(flags[1]),
                ],
            )
            .await
            .map_err(|_| Error::Bus)
    }
}
