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
//! on the back pads: thirteen usable against the eight this design needs.
//!
//! ## What is wired where
//!
//! The whole map in one place, for soldering against. Every row is a macro
//! below, and nothing outside this file may name a pin.
//!
//! | Pin | Goes to | Notes |
//! |---|---|---|
//! | GPIO0 | DRV8833 `AIN1` | edge |
//! | GPIO1 | DRV8833 `AIN2` | edge |
//! | GPIO14 | DRV8833 `nSLEEP` (`ULT`/`SLP`) | edge; high enables the bridge |
//! | GPIO2 | hub microswitch | other side to GND, internal pull-up |
//! | GPIO3 | outside button | other side to GND, internal pull-up |
//! | GPIO8 | WS2812 `DIN` | onboard; also a back pad on the Zero |
//! | GPIO18 | SSD1306 `SDA` | edge |
//! | GPIO19 | SSD1306 `SCL` | edge |
//!
//! Neither switch needs a resistor: both enable the chip's internal pull-up and
//! read a press as a **falling** edge. Power is `3V3` to the display, `5V` to
//! the DRV8833's motor supply, and one ground shared by everything — including
//! the 220 µF sitting across the DRV8833's 5 V and ground.
//!
//! On a bench that `5V` can arrive from two places at once — the feeder's own
//! adapter and a laptop's USB cable — and they meet at the Zero's `5V` pad. It
//! is safe: the board already carries a Schottky between `VBUS` and that pad,
//! so the pad cannot back-feed the laptop, and an external diode would be a
//! second one. See *Two supplies, and one of them is a laptop* in `CLAUDE.md`
//! for the part, why ~4.8 V on the pad is normal, and the one combination that
//! is worth avoiding.
//!
//! GP6, GP7, GP20–GP22 and GP23 stay free, which is the margin for a part that
//! turns out to need a pin nobody planned for.
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

/// DRV8833 `AIN1`. **GPIO0** on both boards.
///
/// One channel drives the motor: `AIN1`/`AIN2` in, `AOUT1`/`AOUT2` out. The B
/// channel is unused and its inputs can be left unconnected — they have
/// internal pull-downs, so that channel stays coasting.
#[macro_export]
macro_rules! motor_in1_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO0
    };
}

/// DRV8833 `AIN2`. **GPIO1** on both boards. See [`motor_in1_pin!`].
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

/// The SSD1306's `SDA`. **GPIO18** on both boards.
///
/// I²C rather than SPI, so the display costs two pins. Both are ordinary GPIOs:
/// the C6 routes peripheral signals through a matrix, so there is no dedicated
/// I²C pair to respect and any free pin does.
///
/// GP18 and GP19 are chosen over GP6/GP7 — also free — because they are **edge
/// castellations rather than back pads**, and this board is hand-soldered.
///
/// ## The bench display is not the one going in the case
///
/// The 0.91" 128×32 modules that fit the LCD window are 4-pin I²C parts:
/// `GND · VCC · SCL · SDA` and nothing else. The 1.3" 128×64 Adafruit breakout
/// being developed against has **eight** pins — `Data · Clk · SA0 · Rst · CS ·
/// 3v3 · Vin · Gnd` — because it speaks SPI as well. `Data` and `Clk` are the
/// same two wires; the rest are mode and address selection.
///
/// Two differences survive the swap and neither is the size:
///
/// - **The I²C address.** Adafruit's 128×64 answers on `0x3D` by default, while
///   the 0.91" modules answer on `0x3C`. Do not hardcode either — scan, log
///   what answered, and take it from there.
/// - **`Rst` and `CS`.** The 4-pin modules have neither. On the breakout, newer
///   revisions ship with the I²C jumpers closed and an auto-reset circuit, so
///   both can be left alone; older ones want `CS` at ground and the jumpers
///   soldered. If nothing answers a scan, that is the first thing to check —
///   before suspecting these two pins.
///
/// ⚠️ **An SSD1306 will ACK its address with no ground connected, and stay
/// dark.** Cost an evening. With the `Gnd` pin unwired, the board still finds a
/// return path through the ESD protection diodes on `SDA`, `SCL` and anything
/// else tied to the ground rail — `CS`, in the case here. That is enough to
/// power the logic, so the address ACKs, the whole init sequence is accepted
/// and every flush succeeds. It is nowhere near enough for the charge pump that
/// makes the ~7.5 V the OLED matrix needs, so not one pixel lights.
///
/// The symptom is therefore a driver that reports success at every step next to
/// a blank panel, which reads as a software fault and is not one. A missing
/// ground usually announces itself by nothing working at all; this is the
/// nastier presentation. `Gnd` is the only pin built to carry that return —
/// grounding `CS` does not substitute for it, and pushing supply current
/// through a protection diode stresses a structure meant for static discharge.
#[macro_export]
macro_rules! display_sda_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO18
    };
}

/// The SSD1306's `SCL`. **GPIO19** on both boards. See [`display_sda_pin!`].
#[macro_export]
macro_rules! display_scl_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO19
    };
}

/// See [`MOTOR_IN1_PIN`].
pub const DISPLAY_SDA_PIN: &str = "GPIO18";
/// See [`MOTOR_IN1_PIN`].
pub const DISPLAY_SCL_PIN: &str = "GPIO19";
