//! Pin map and board identity.
//!
//! Every pin number lives here and nowhere else, so bringing up a second board
//! is a change to this file alone.
//!
//! Pins are handed out by macro rather than by function because esp-hal's
//! peripheral singletons are moved, not borrowed, so a plain accessor taking
//! `&mut Peripherals` cannot give one away.
//!
//! ## The two boards
//!
//! | | `board-devkit` (default) | `board-zero` |
//! |---|---|---|
//! | Part | ESP32-C6-DEV-KIT-N8 | ESP32-C6-Zero ×3 |
//! | Role | bench only | in the feeders |
//! | USB | WCH bridge chip | the chip's own USB |
//! | `esp-println` | `uart` | `jtag-serial` |
//!
//! ```sh
//! cargo run                                                   # dev kit
//! cargo run --no-default-features --features board-zero       # a Zero
//! ```
//!
//! `--no-default-features` is not optional. Cargo features are additive, so
//! asking for `board-zero` without it leaves `board-devkit` on as well, and
//! `esp-println` would be told to use two output interfaces at once.
//!
//! Its build script catches that, and says so clearly:
//!
//! ```text
//! Exactly one of the following features must be enabled: jtag-serial, uart, auto, no-op.
//! Currently enabled: jtag-serial, uart. This might be caused by enabled default features.
//! ```
//!
//! `Currently enabled: none` is the same mistake the other way round —
//! `--no-default-features` with no board feature to replace it.
//!
//! This module deliberately carries no `compile_error!` guard of its own.
//! A dependency's build script runs before this crate is compiled, so
//! `esp-println` always fails first and such a guard could never run; it would
//! be unreachable code claiming to catch something it cannot.
//!
//! The serial port changes too, and the scripts take it from the environment:
//!
//! ```sh
//! espflash list-ports --list-all-ports
//! ESPFLASH_PORT=/dev/cu.usbmodemXXXX ./dev/flash.sh
//! ```
//!
//! ## Which pins are usable
//!
//! The Zero is the binding constraint, because it brings out fewer pads than
//! the dev kit brings out header pins. From its pad map: GP0–GP9 and GP12–GP23,
//! with GP16/GP17 appearing as `TX`/`RX`. **GPIO10 and GPIO11 are not brought
//! out at all** — neither on the edge castellations nor on the back pad row.
//!
//! Subtract the pins that are already spoken for:
//!
//! | Pin | Why not |
//! |---|---|
//! | GPIO4, GPIO5, GPIO8, GPIO9, GPIO15 | strapping, sampled at reset |
//! | GPIO12, GPIO13 | native USB D−/D+; on the Zero, the only console there is |
//! | GPIO8 | also the onboard WS2812, so already committed |
//!
//! That leaves GP0–GP3, GP14 and GP18–GP22 on the edge, plus GP6, GP7 and GP23
//! on the back pads: thirteen usable against the seven this design needs.
//!
//! "No alternate function" was the rule that originally picked GPIO10 and
//! GPIO11 on the dev kit. It does not really apply on the C6, where peripheral
//! signals route through a GPIO matrix and the labels on a pinout diagram are a
//! convention rather than a restriction. Any pin outside the table above will
//! do.

/// Printed at boot next to the device id, so a console says which board it is
/// looking at before anything else can go wrong.
#[cfg(feature = "board-devkit")]
pub const NAME: &str = "devkit";
#[cfg(all(feature = "board-zero", not(feature = "board-devkit")))]
pub const NAME: &str = "zero";

/// The hub microswitch. **GPIO2** on both boards.
///
/// Wired to GND through the switch with the internal pull-up enabled, so a
/// press is a falling edge. See `CLAUDE.md` for the wiring, including the
/// warning about the ground pin sitting next to 5V on the dev kit.
///
/// This was GPIO11 until the Zero's pad map was checked against it: GPIO11 is
/// not brought out on that board, and neither is GPIO10, which the reset button
/// had been given. Both moved rather than diverging per board, so the bench
/// tests the same wiring production runs.
///
/// On the dev kit's J1 header the move is one position: the header reads
/// `5V · GPIO3 · GPIO2 · GPIO11`, so the jumper shifts by a single pin.
#[macro_export]
macro_rules! switch_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO2
    };
}

/// What [`switch_pin!`] resolves to, for the log line that says so at boot.
///
/// Kept beside the macro so the console can never disagree with the wiring.
pub const SWITCH_PIN: &str = "GPIO2";

/// The config-reset button. **GPIO3** on both boards. Roadmap step 9.
///
/// Separate from the hub microswitch on purpose: that one is inside the
/// mechanism and unreachable once a feeder is assembled, and this one has to be
/// pressable from outside the case.
///
/// ⚠️ On the dev kit's J1 header GPIO3 is the pin **directly beside 5V**. That
/// is the same adjacency `CLAUDE.md` warns about for the ground jumper, and it
/// is worth re-reading before wiring a button there. If it makes you nervous,
/// any of GP14 or GP18–GP22 is free on the Zero and this is a one-line change.
#[macro_export]
macro_rules! button_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO3
    };
}

/// See [`SWITCH_PIN`].
pub const BUTTON_PIN: &str = "GPIO3";

/// The onboard WS2812 RGB LED. **GPIO8** on both boards.
///
/// GPIO8 is a strapping pin, which does not matter here: strapping is sampled
/// at reset and this drives it as an output long afterwards. The LED is wired
/// to it by the board vendor either way, so there is no choice to make.
///
/// **On the Zero, GPIO8 is also exposed on the back pad row.** An external
/// WS2812 wired to that pad sits in parallel on the same data line: both LEDs
/// independently latch the first 24 bits and show the same colour. That is how
/// an indicator gets outside a closed case without spending a second pin or a
/// line of firmware.
#[macro_export]
macro_rules! led_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO8
    };
}

// When the DRV8833 arrives, its IN1, IN2 and nSLEEP pins belong in this file
// too, as macros alongside `switch_pin!`. GP0, GP1 and GP14 are free and sit on
// the Zero's edge castellations, which are easier to hand-solder than the back
// pads. `nSLEEP` can be strapped high to 3V3 instead if a pin is ever needed
// back.
