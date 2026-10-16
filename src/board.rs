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
//! The serial port changes too, and the scripts take it as a flag:
//!
//! ```sh
//! espflash list-ports --list-all-ports
//! ./dev/flash --port /dev/cu.usbmodemXXXX
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
//! | GPIO8, GPIO9, GPIO15 | strapping that matters: boot mode, boot log, JTAG source |
//! | GPIO12, GPIO13 | native USB D−/D+; on the Zero, the only console there is |
//! | GPIO8 | also the onboard WS2812, so already committed |
//!
//! That leaves GP0–GP5, GP14 and GP18–GP22 on the edge, plus GP6, GP7 and GP23
//! on the back pads: **fifteen usable, six of them spent.**
//!
//! Six and not eight, though the table below has eight rows: GPIO8 and GPIO9
//! are not among the fifteen. GPIO8 is struck out twice just above, as
//! strapping and as the onboard WS2812; GPIO9 is strapping and the onboard
//! BOOT button. Both are wired, but neither was ever available to spend.
//!
//! **GPIO4 and GPIO5 are strapping pins too, and usable anyway.** They are
//! `MTMS`/`MTDI`, and on the C6 they only choose the SDIO slave's sampling and
//! driving edges. The boot mode is GPIO8 and GPIO9, so whatever a part holds
//! these two at during reset cannot stop the unit booting from flash.
//!
//! ## What is wired where
//!
//! The whole map in one place, for soldering against. Every row is a macro
//! below, and nothing outside this file may name a pin.
//!
//! | Pin | Goes to | Notes |
//! |---|---|---|
//! | GPIO0 | DRV8833 `IN1` of either channel | edge; `In1` on the perfboard |
//! | GPIO1 | DRV8833 `IN2` of the same channel | edge; `In2` on the perfboard |
//! | GPIO14 | DRV8833 `nSLEEP` (`ULT`/`SLP`) | edge; high enables the bridge |
//! | GPIO2 | hub microswitch | other side to GND, internal pull-up; 1 kΩ in series on the perfboard |
//! | GPIO8 | WS2812 `DIN` | onboard; also a back pad on the Zero |
//! | GPIO9 | the onboard **BOOT** button | read only *after* boot: held 5 s, forgets the network — see `reset.rs`. A back pad on the Zero, so an external button can sit in parallel |
//! | GPIO18 | DS3231 `SDA`, on `0x68` | edge; see `i2c.rs` |
//! | GPIO19 | DS3231 `SCL` | edge |
//!
//! No switch needs a pull-up resistor: each enables the chip's internal one and
//! reads a press as a **falling** edge. The 1 kΩ on GPIO2 is not a pull-up but
//! a guard: the feeder's cable is an unkeyed header with the switch beside a
//! motor wire, and it keeps a plug put on wrong from reaching the pin with 5 V.
//! Power is `3V3` to the RTC, `5V` to the DRV8833's motor
//! supply, and one ground shared by everything — with the 220 µF across the
//! driver's 5 V and ground, which on the perfboard is also the star point for
//! both. See *The perfboard* in `docs/hardware.md`.
//!
//! On a bench that `5V` can arrive from two places at once — the feeder's own
//! adapter and a laptop's USB cable — and they meet at the Zero's `5V` pad. It
//! is safe: the board already carries a Schottky between `VBUS` and that pad,
//! so the pad cannot back-feed the laptop, and an external diode would be a
//! second one. See *A laptop on USB at the same time* in `docs/hardware.md`
//! for the part, why ~4.8 V on the pad is normal, and the one combination that
//! is worth avoiding.
//!
//! GP3–GP7 and GP20–GP23 stay free, which is the margin for a part that turns
//! out to need a pin nobody planned for.
//!
//! The DS3231 RTC is on I²C, two pins. Power it from `3V3`: the common
//! breakouts charge their coin cell from `VCC`, which suits the LIR2032 fitted
//! here at 3.3 V and overcharges it at 5 V.
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
/// press is a falling edge. See `docs/hardware.md` for the wiring, including the
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

/// The onboard **BOOT** button. **GPIO9** on both boards, with its own pull-up
/// on the board; the internal one is enabled as well.
///
/// A strapping pin, and that is exactly why it is safe to read at runtime and
/// no other time: held *through* a reset it selects download mode, and the
/// firmware never runs to see it. Pressed while the firmware runs, it is an
/// ordinary input. Held for five seconds it forgets the network settings and
/// reboots into setup mode — the one recovery that needs no network. It costs
/// no pin: it is already on every board, and struck out of the usable list
/// above for the same strapping reason.
#[macro_export]
macro_rules! boot_button_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO9
    };
}

/// See [`SWITCH_PIN`].
pub const BOOT_BUTTON_PIN: &str = "GPIO9";

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

/// DRV8833 `IN1` of the channel in use. **GPIO0** on both boards.
///
/// One channel drives the motor, and it does not matter which: the breadboard
/// and `pcb.diy` both use A (`In1`/`In2` in, `Out1`/`Out2` out). The other channel's inputs can be left unconnected — they
/// have internal pull-downs, so it stays coasting.
#[macro_export]
macro_rules! motor_in1_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO0
    };
}

/// DRV8833 `IN2` of the same channel. **GPIO1** on both boards. See
/// [`motor_in1_pin!`].
#[macro_export]
macro_rules! motor_in2_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO1
    };
}

/// DRV8833 `nSLEEP`, labelled `ULT` or `SLP` on some breakouts. **GPIO14**.
///
/// Driven high to enable the bridge, low to sleep it. It could be strapped to
/// 3V3 instead — the comment this replaced suggested exactly that — but a GPIO
/// is worth the pin, because it makes "the motor is off" a state the firmware
/// asserts rather than one it merely refrains from disturbing.
///
/// **An ESP32 pin floats until firmware configures it**, and that is the case
/// this choice is really about. The DRV8833 pulls `nSLEEP` and both inputs down
/// internally, so from power-on until [`crate::motor::Drv8833::new`] runs,
/// the bridge is asleep
/// and the outputs are coasting. Strapping `nSLEEP` high removes that margin:
/// the bridge is live through the whole boot, and only the input pull-downs
/// stand between a floating pin and a hopper being emptied.
#[macro_export]
macro_rules! motor_sleep_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO14
    };
}

/// See [`SWITCH_PIN`]. Printed together as one line at boot.
pub const MOTOR_IN1_PIN: &str = "GPIO0";
/// See [`MOTOR_IN1_PIN`].
pub const MOTOR_IN2_PIN: &str = "GPIO1";
/// See [`MOTOR_IN1_PIN`].
pub const MOTOR_SLEEP_PIN: &str = "GPIO14";

/// The DS3231's `SDA`. **GPIO18** on both boards.
///
/// Both I²C lines are ordinary GPIOs: the C6 routes peripheral signals through
/// a matrix, so there is no dedicated I²C pair to respect and any free pin
/// does. GP18 and GP19 are chosen over GP6/GP7 — also free — because they are
/// **edge castellations rather than back pads**, and this board is
/// hand-soldered.
#[macro_export]
macro_rules! i2c_sda_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO18
    };
}

/// The DS3231's `SCL`. **GPIO19** on both boards. See [`i2c_sda_pin!`].
#[macro_export]
macro_rules! i2c_scl_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO19
    };
}

/// See [`MOTOR_IN1_PIN`].
pub const I2C_SDA_PIN: &str = "GPIO18";
/// See [`MOTOR_IN1_PIN`].
pub const I2C_SCL_PIN: &str = "GPIO19";
