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
//! Async is the right half here. A 128×64 frame is 1 KB, which at 400 kHz
//! is several milliseconds on the wire, and a *blocking* write inside an
//! Embassy task stalls every other task on the executor — including the one
//! holding the `next_click` future that counts portions. Nothing about a screen
//! is worth dropping a click for.
//!
//! ## The address is discovered, not assumed
//!
//! An SSD1306 answers on `0x3C` or `0x3D`, selected on the module by a jumper
//! or a resistor. The 0.96" SSD1315 on the bench is `0x3C`; the 1.3" Adafruit
//! breakout tried before it is usually `0x3D`. A replacement part may be
//! either, which is the point: neither address can be assumed.
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
use embedded_hal_async::i2c::I2c as _;
use log::{info, warn};

use crate::i2c::Device;
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

/// Row height of `FONT_6X10`. Six of them fill a 128×64 panel with 4 px over.
const LINE_H: i32 = 10;

/// The 0.96" 128×64. `ssd1306` takes the size as a type, and it is not
/// cosmetic — it sets the multiplex ratio, so a 128×64 panel initialised as
/// 128×32 shows a garbled half-height image rather than a small one. The
/// 0.91" 128×32 used to be selectable here and was dropped as a fallback once
/// this part proved itself; `display.rs` now lays out six rows.
///
/// The bench part is an **SSD1315**, which is register-compatible with the
/// SSD1306 this driver is written for.
type PanelSize = DisplaySize128x64;
const PANEL: PanelSize = DisplaySize128x64;

type Panel<'d> =
    Ssd1306Async<I2CInterface<Device<'d>>, PanelSize, BufferedGraphicsModeAsync<PanelSize>>;

/// Why a panel could not be brought up.
#[derive(Debug)]
pub enum Error {
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

impl<'d> Oled<'d> {
    /// Brings up the panel, probing both addresses.
    ///
    /// Takes a handle on the shared bus rather than the bus, because the RTC
    /// is on the same two wires. See `i2c.rs`.
    pub async fn new(mut bus: Device<'d>) -> Result<Self, Error> {
        let address = probe(&mut bus).await.ok_or(Error::NotFound)?;
        info!("oled: found a panel at {address:#04x}");

        let interface = I2CDisplayInterface::new_custom_address(bus, address);
        let mut panel = Ssd1306Async::new(interface, PANEL, DisplayRotation::Rotate0)
            .into_buffered_graphics_mode();

        panel.init().await.map_err(|_| Error::Init)?;

        // **Blank it before anyone can look at it.**
        //
        // The SSD1306 powers up with its display RAM *undefined*, and `init`
        // ends with the display switched on. Nothing in the sequence clears
        // GDDRAM, so a freshly initialised panel shows whatever the RAM
        // happened to contain — scattered lit pixels, which reads as a broken
        // screen rather than as an uninitialised one. It stayed that way until
        // `display_task` first drew, which on the configured boot path is
        // after the flash record is read and every other task is spawned.
        //
        // Off, clear, flush, on — rather than just clear and flush — because
        // pushing a 1 KB frame at 400 kHz takes over twenty milliseconds
        // and the datasheet is explicit that the buffer can be written while
        // the display is off. This way the noise is never scanned out at all,
        // instead of being shown briefly on every boot.
        //
        // Errors here are deliberately not fatal, for the same reason `show`
        // swallows its own: a feeder with an unreadable panel still feeds cats.
        // A panel that will not blank is one that will not draw either, and
        // that shows up in `show`'s warning a moment later.
        let _ = panel.set_display_on(false).await;
        panel.clear_buffer();
        let _ = panel.flush().await;
        let _ = panel.set_display_on(true).await;

        Ok(Self {
            panel,
            style: MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        })
    }

    /// Draws one screen and pushes it.
    ///
    /// The whole frame every time rather than a diff. At 1 KB it is not
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
async fn probe(bus: &mut Device<'_>) -> Option<u8> {
    // The embedded-hal-async trait's `write`, through the shared-bus handle —
    // which is async all the way down, unlike the raw peripheral, whose
    // inherent blocking `write` used to win name resolution here.
    for address in ADDRESSES {
        if bus.write(address, &[0x00]).await.is_ok() {
            return Some(address);
        }
    }

    None
}
