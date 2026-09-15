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

/// Printed at boot next to the device id, so a console says which board it is
/// looking at before anything else can go wrong.
#[cfg(feature = "board-devkit")]
pub const NAME: &str = "devkit";
#[cfg(all(feature = "board-zero", not(feature = "board-devkit")))]
pub const NAME: &str = "zero";

/// The hub microswitch.
///
/// Wired to GND through the switch with the internal pull-up enabled, so a
/// press is a falling edge. See `CLAUDE.md` for the wiring, including the
/// warning about the ground pin sitting next to 5V on the dev kit.
///
/// **Dev kit:** GPIO11, on the J1 header, third pin in from 5V. Verified on
/// hardware. It and GPIO10 are the only header pins with no alternate function
/// at all, which is why the switch gets one of them.
///
/// **Zero:** GPIO11 as well, so both boards are wired the same. This has
/// **not** been checked against the Zero's pad map — confirm the pad exists
/// before wiring a production unit. If it does not, change the number here and
/// nowhere else.
#[macro_export]
macro_rules! switch_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO11
    };
}

// When the DRV8833 arrives, its IN1, IN2 and nSLEEP pins belong in this file
// too, as macros alongside `switch_pin!`.
//
// Two constraints apply to both boards before picking them:
//
//   * GPIO8, 9 and 15 are strapping pins. GPIO8 is also the onboard RGB LED on
//     both boards, and is the intended "feeding" indicator.
//   * GPIO12 and GPIO13 are the ESP32-C6's native USB D- and D+. On the Zero
//     they carry the only serial console there is, so using them for the motor
//     would silence the board.
