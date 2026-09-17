//! The SSD1306 itself, over I²C. Text in, pixels out.
//!
//! [`crate::display`] decides *what* to show and is host-tested; this module
//! only draws it. The split is the same one `indicator.rs` and `led.rs` have,
//! and for the same reason: everything worth arguing about is in the pure half.
//!
//! ## Asynchronous on purpose
//!
//! `ssd1306`'s `async` feature swaps the crate's whole API from `embedded-hal`
//! to `embedded-hal-async`, and esp-hal implements both — blocking for any
//! driver mode, async for `I2c<'_, Async>`.
//!
//! Async is the right half here. A 128×32 frame is 512 bytes, which at 400 kHz
//! is several milliseconds on the wire, and a *blocking* write inside an
//! Embassy task stalls every other task on the executor — including the one
//! holding the `next_click` future that counts portions. Nothing about a screen
//! is worth dropping a click for.
//!
//! ## The address is discovered, not assumed
//!
//! An SSD1306 answers on `0x3C` or `0x3D`, selected on the module by a jumper
//! or a resistor. The 0.91" parts that fit the case are `0x3C`; the 1.3"
//! Adafruit breakout being developed against is usually `0x3D`.
//!
//! Hardcoding either gives a driver that works on the bench and shows nothing
//! the day the real panel is fitted — with no clue as to why, because a device
//! that is not there simply never ACKs. So [`Oled::new`] probes both and says
//! which answered.

use crate::display::Screen;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use esp_hal::gpio::interconnect::{PeripheralInput, PeripheralOutput};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::time::Rate;
use esp_hal::{Async, peripherals};
use log::{info, warn};
// The async half of the crate, which `maybe-async-cfg` generates as a parallel
// set of `*Async` items: the blocking names are kept (`sync(keep_self)`) and the
// async ones are suffixed. The concrete sizes are the exception -- `size.rs:76`
// passes `keep_self` on both arms -- and so is `I2CDisplayInterface`, whose
// constructors are switched by plain `#[cfg]` instead.
use ssd1306::mode::{BufferedGraphicsModeAsync, DisplayConfigAsync};
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306Async};

/// The two addresses an SSD1306 can be strapped to. Probed in this order.
pub const ADDRESSES: [u8; 2] = [0x3C, 0x3D];

/// 400 kHz. The panel handles it, and it is four times less time on the wire
/// than the 100 kHz default — which matters because every millisecond here is a
/// millisecond the I²C peripheral is busy.
const BUS_HZ: u32 = 400;

/// Row height of `FONT_6X10`. Three of them fill a 128×32 panel with 2 px over.
const LINE_H: i32 = 10;

#[cfg(not(feature = "panel-128x64"))]
type PanelSize = DisplaySize128x32;
#[cfg(feature = "panel-128x64")]
type PanelSize = DisplaySize128x64;

#[cfg(not(feature = "panel-128x64"))]
const PANEL: PanelSize = DisplaySize128x32;
#[cfg(feature = "panel-128x64")]
const PANEL: PanelSize = DisplaySize128x64;

type Panel<'d> =
    Ssd1306Async<I2CInterface<I2c<'d, Async>>, PanelSize, BufferedGraphicsModeAsync<PanelSize>>;

/// Why a panel could not be brought up.
#[derive(Debug)]
pub enum Error {
    /// The I²C peripheral itself would not configure.
    Bus(esp_hal::i2c::master::ConfigError),
    /// Nothing ACKed on either address. Usually no panel wired, the two lines
    /// swapped, or a module whose I²C jumpers were never closed.
    NotFound,
    /// Something answered, then failed the initialisation sequence.
    Init,
}

pub struct Oled<'d> {
    panel: Panel<'d>,
    style: MonoTextStyle<'static, BinaryColor>,
}

impl Oled<'static> {
    /// Brings up the panel, probing both addresses.
    ///
    /// Takes the pins rather than a built bus, so the whole I²C setup lives
    /// here and `main.rs` stays wiring.
    pub async fn new(
        i2c: peripherals::I2C0<'static>,
        sda: impl PeripheralInput<'static> + PeripheralOutput<'static>,
        scl: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    ) -> Result<Self, Error> {
        let mut bus = I2c::new(
            i2c,
            Config::default().with_frequency(Rate::from_khz(BUS_HZ)),
        )
        .map_err(Error::Bus)?
        .with_sda(sda)
        .with_scl(scl)
        .into_async();

        let address = probe(&mut bus).await.ok_or(Error::NotFound)?;
        info!("oled: found a panel at {address:#04x}");

        let interface = I2CDisplayInterface::new_custom_address(bus, address);
        let mut panel = Ssd1306Async::new(interface, PANEL, DisplayRotation::Rotate0)
            .into_buffered_graphics_mode();

        panel.init().await.map_err(|_| Error::Init)?;

        Ok(Self {
            panel,
            style: MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        })
    }

    /// Draws one screen and pushes it.
    ///
    /// The whole frame every time rather than a diff. At 512 bytes it is not
    /// worth tracking dirty regions, and the caller already refuses to call
    /// this unless something changed.
    pub async fn show(&mut self, screen: &Screen) {
        self.panel.clear_buffer();

        for (row, line) in screen.lines().enumerate() {
            if line.is_empty() {
                continue;
            }

            // `Baseline::Top` so a row's y is its top edge, which makes the
            // arithmetic here the same as the one in `display.rs`'s docs.
            let at = Point::new(0, row as i32 * LINE_H);
            if Text::with_baseline(line, at, self.style, Baseline::Top)
                .draw(&mut self.panel)
                .is_err()
            {
                warn!("oled: line {row} would not fit the buffer");
            }
        }

        if self.panel.flush().await.is_err() {
            // Not fatal and deliberately not a panic: a feeder with a dead
            // screen still feeds cats, which is the same call `led.rs` makes.
            warn!("oled: flush failed, panel may be unplugged");
        }
    }

    /// Lights the panel or blanks it.
    ///
    /// One command, and the frame buffer survives it — the datasheet's own
    /// wording is that the display "can be drawn to and retains all of its
    /// memory even while off". So waking is instant and needs no redraw, which
    /// is what makes blanking cheap enough to do on a thirty-second timer.
    pub async fn set_power(&mut self, on: bool) {
        if self.panel.set_display_on(on).await.is_err() {
            warn!(
                "oled: could not turn the panel {}",
                if on { "on" } else { "off" }
            );
        }
    }
}

/// Returns the first address that ACKs.
///
/// A one-byte write of `0x00` — the SSD1306's command prefix, and a no-op the
/// panel is happy to receive. What is being tested is the address phase, not
/// the payload: a device that is not there never ACKs and the write errors.
async fn probe(bus: &mut I2c<'static, Async>) -> Option<u8> {
    // esp-hal's *inherent* async method (src/i2c/master/mod.rs:987), not the
    // embedded-hal-async trait. The inherent blocking `write` would otherwise
    // win name resolution and quietly stall the executor for the probe.
    for address in ADDRESSES {
        if bus.write_async(address, &[0x00]).await.is_ok() {
            return Some(address);
        }
    }

    None
}
