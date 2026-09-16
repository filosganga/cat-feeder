//! The onboard WS2812, driven from RMT.
//!
//! Nothing but bytes on a wire: every decision about *what* to show lives in
//! [`crate::indicator`], which is pure and host-tested. This module knows one
//! thing the tests cannot check — the protocol timing.
//!
//! ## Why RMT and not a crate
//!
//! `esp-hal-smartled` exists and would do this, but it tracks esp-hal versions
//! the same way `esp-storage` does, and this project has already been bitten
//! once by a companion crate demanding a newer HAL than the one pinned. The
//! encoder below is two dozen lines against `esp_hal::rmt`, which is in 1.1.2
//! and needs nothing added to `Cargo.toml`.
//!
//! ## The protocol
//!
//! A WS2812 reads 24 bits as GRB, most significant bit first, where a bit is a
//! single high-then-low pulse and the *ratio* carries the value. At 80 MHz with
//! no divider one RMT tick is 12.5 ns, and the four pulse widths land on whole
//! ticks with room to spare inside the part's ±150 ns tolerance.
//!
//! The latch is the gap afterwards: hold the line low for >50 µs and the LED
//! commits what it just heard. Nothing here sends one, because the caller only
//! writes on a colour change and those are milliseconds apart at the closest —
//! the idle line between transmissions is the latch, three orders of magnitude
//! longer than the minimum.
//!
//! ## Two LEDs, one wire
//!
//! On the Zero, GPIO8 is both the onboard WS2812's DIN *and* a pad on the back
//! row. An external WS2812 wired to that pad sits in parallel on this same
//! signal: each part independently latches the first 24 bits it hears, so both
//! show the same colour. That is deliberate and is how an indicator gets
//! outside a sealed feeder — no second pin, no second channel, and not one line
//! of difference here. Sending 24 bits rather than 48 is what makes it work.

use esp_hal::Async;
use esp_hal::gpio::Level;
use esp_hal::gpio::interconnect::PeripheralOutput;
use esp_hal::peripherals::RMT;
use esp_hal::rmt::{Channel, PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator};
use esp_hal::time::Rate;

use crate::indicator::Rgb;

/// The RMT source clock. One tick is 12.5 ns at this rate with no divider.
const RMT_MHZ: u32 = 80;

/// Nanoseconds to RMT ticks.
///
/// Exact for every value used below: all four are multiples of 12.5 ns.
const fn ticks(ns: u32) -> u16 {
    (ns * RMT_MHZ / 1000) as u16
}

// WS2812B datasheet timings, ±150 ns.
const T0H: u16 = ticks(400);
const T0L: u16 = ticks(850);
const T1H: u16 = ticks(800);
const T1L: u16 = ticks(450);

/// 24 bits, plus the marker that tells RMT the sequence is over.
const CODES: usize = 25;

/// The 24-bit word, in the order this part wants it on the wire.
///
/// **Red first, not green** — which contradicts the WS2812B datasheet, and is
/// the empirical answer for the LED on the dev kit.
///
/// This code originally sent GRB, because that is what the datasheet specifies.
/// On hardware every red pattern rendered green and the green confirmation
/// rendered red: the three network fault codes blinked green, and a jam sat
/// solid green. A consistent, stable swap like that is a channel-order fault
/// and nothing else — marginal pulse timing produces flicker and random
/// colours, not a clean substitution.
///
/// **If a Zero shows red and green swapped, this is the line to change**, and
/// it may have to become per-board. The two boards are not guaranteed to carry
/// the same LED part, and nothing in the firmware can detect which is fitted.
/// `dev/led-selftest` in `main.rs` names each colour as it shows it, which is
/// how to re-check in one flash.
const fn wire_word(colour: Rgb) -> u32 {
    (colour.r as u32) << 16 | (colour.g as u32) << 8 | colour.b as u32
}

/// Drives one colour onto the LED (or onto every LED sharing the pin).
pub struct Led<'d> {
    channel: Channel<'d, Async, Tx>,
    codes: [PulseCode; CODES],
}

impl<'d> Led<'d> {
    /// Claims RMT channel 0 and the LED pin.
    ///
    /// Fails only if RMT will not accept the requested clock, which is a
    /// programming error rather than a runtime condition; the caller logs it
    /// and carries on without an LED rather than refusing to feed the cats.
    pub fn new(
        rmt: RMT<'d>,
        pin: impl PeripheralOutput<'d>,
    ) -> Result<Self, esp_hal::rmt::ConfigError> {
        let rmt = Rmt::new(rmt, Rate::from_mhz(RMT_MHZ))?.into_async();

        let channel = rmt
            .channel0
            .configure_tx(&TxChannelConfig::default().with_clk_divider(1))?
            .with_pin(pin);

        Ok(Self {
            channel,
            codes: [PulseCode::end_marker(); CODES],
        })
    }

    /// Shows `colour`, and does not return until the bits are on the wire.
    ///
    /// Errors are swallowed: a failed transmission costs one frame of an
    /// indicator, and the next call overwrites it anyway. There is nothing
    /// useful to do about it and logging every occurrence would be worse than
    /// the fault.
    pub async fn set(&mut self, colour: Rgb) {
        let word = wire_word(colour);

        for (i, code) in self.codes[..24].iter_mut().enumerate() {
            // Most significant bit first: bit 23 down to bit 0.
            *code = if word & (1 << (23 - i)) != 0 {
                PulseCode::new(Level::High, T1H, Level::Low, T1L)
            } else {
                PulseCode::new(Level::High, T0H, Level::Low, T0L)
            };
        }

        let _ = self.channel.transmit(&self.codes).await;
    }
}
