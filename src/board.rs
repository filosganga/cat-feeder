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
//! | GPIO8, GPIO9, GPIO15 | strapping that matters: boot mode, boot log, JTAG source |
//! | GPIO12, GPIO13 | native USB D−/D+; on the Zero, the only console there is |
//! | GPIO8 | also the onboard WS2812, so already committed |
//!
//! That leaves GP0–GP5, GP14 and GP18–GP22 on the edge, plus GP6, GP7 and GP23
//! on the back pads: **fifteen usable, nine of them spent today.**
//!
//! Nine and not eleven, though the table below has eleven rows: GPIO8 and
//! GPIO9 are not among the fifteen. GPIO8 is struck out twice just above, as
//! strapping and as the onboard WS2812; GPIO9 is strapping and the onboard
//! BOOT button. Both are wired, but neither was ever available to spend. Count
//! the free pins against nine or the arithmetic comes out short. The headless
//! build spends six: GPIO3–GPIO5 go with the knob.
//!
//! **GPIO4 and GPIO5 are strapping pins too, and are spent anyway.** They are
//! `MTMS`/`MTDI`, and on the C6 they only choose the SDIO slave's sampling and
//! driving edges. The boot mode is GPIO8 and GPIO9, so whatever an encoder
//! holds these two at during reset cannot stop the unit booting from flash.
//! This table used to strike them out alongside the three that matter. That was
//! more cautious than the chip requires, and the encoder is now wired to them.
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
//! | GPIO3 | encoder push switch | the outside button; other side to GND, internal pull-up. Unwired and unread on the headless build |
//! | GPIO4 | encoder `A` (`CLK`) | strapping, harmless — see above; internal pull-up |
//! | GPIO5 | encoder `B` (`DT`) | strapping, harmless — see above; internal pull-up |
//! | GPIO8 | WS2812 `DIN` | onboard; also a back pad on the Zero |
//! | GPIO9 | the onboard **BOOT** button | read only *after* boot: held 5 s, forgets the network — see `reset.rs`. A back pad on the Zero, so an external button can sit in parallel |
//! | GPIO18 | `SDA`: the panel on `0x3C` and the DS3231 on `0x68` | edge; one bus, see `i2c.rs` |
//! | GPIO19 | `SCL`: the same two devices | edge |
//!
//! No switch needs a pull-up resistor: each enables the chip's internal one and
//! reads a press as a **falling** edge. The 1 kΩ on GPIO2 is not a pull-up but
//! a guard: the feeder's cable is an unkeyed header with the switch beside a
//! motor wire, and it keeps a plug put on wrong from reaching the pin with 5 V.
//! Power is `3V3` to the display and the RTC, `5V` to the DRV8833's motor
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
//! GP6, GP7, GP20–GP23 stay free, which is the margin for a part that turns
//! out to need a pin nobody planned for.
//!
//! ## The encoder is the outside button
//!
//! An EC11-style rotary encoder with a push switch in the shaft. Of the two
//! options in `docs/adr/0010-the-knob-is-the-outside-control.md`, this is the one taken: the
//! shaft switch *is* the outside button, on GPIO3. A hold opens or closes the
//! menu and a tap runs the item under the cursor — see `menu.rs` — and the
//! power-on gesture that forgets the network is unchanged. GP20 and GP21 used to be reserved for `A`/`B`;
//! they went to GPIO4 and GPIO5 instead, and the reservation is released.
//!
//! It is a bare encoder, not a breakout: its common pin and the switch's other
//! leg go to GND, and nothing goes to a supply. So `A` and `B` are wired
//! exactly like the two switches — each contact pulls its pin to ground — and
//! each gets the internal pull-up, as GPIO2 and GPIO3 do. No capacitors: the
//! decoder in `encoder.rs` cancels bounce by construction.
//!
//! ⚠️ **If it is ever swapped for a KY-040-style module**, power that module's
//! `+` from `3V3`, never `5V`. Those carry 10 kΩ pull-ups from `A` and `B` to
//! `+`, so a module on 5 V holds two GPIOs at 5 V, and the C6 is not 5 V
//! tolerant.
//!
//! The DS3231 RTC costs no pin at all — it shares the display's bus. Power it
//! from `3V3`: the common breakouts charge their coin cell from `VCC`, which
//! suits the LIR2032 fitted here at 3.3 V and overcharges it at 5 V.
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

/// The outside button: the rotary encoder's push switch. **GPIO3** on both
/// boards. It arms, feeds and locks at runtime, and forgets the network when
/// held through power-on — see `button.rs`. Not read at all on the headless
/// build.
///
/// Separate from the hub microswitch on purpose: that one is inside the
/// mechanism and unreachable once a feeder is assembled, and this one has to be
/// pressable from outside the case.
///
/// ⚠️ On the dev kit's J1 header GPIO3 is the pin **directly beside 5V**. That
/// is the same adjacency `docs/hardware.md` warns about for the ground jumper, and it
/// is worth re-reading before wiring a button there. If it makes you nervous,
/// GP20–GP22 are free edge pads on the Zero and this is a one-line change.
#[macro_export]
macro_rules! button_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO3
    };
}

/// See [`SWITCH_PIN`].
pub const BUTTON_PIN: &str = "GPIO3";

/// The onboard **BOOT** button. **GPIO9** on both boards, with its own pull-up
/// on the board; the internal one is enabled as well.
///
/// A strapping pin, and that is exactly why it is safe to read at runtime and
/// no other time: held *through* a reset it selects download mode, and the
/// firmware never runs to see it. Pressed while the firmware runs, it is an
/// ordinary input. Held for five seconds it forgets the network settings and
/// reboots into setup mode — the one recovery that needs no knob and no
/// network, which is what a headless unit relies on. It costs no pin: it is
/// already on every board, and struck out of the usable list above for the
/// same strapping reason.
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

/// The SSD1306's `SDA`. **GPIO18** on both boards.
///
/// I²C rather than SPI, so the display costs two pins. Both are ordinary GPIOs:
/// the C6 routes peripheral signals through a matrix, so there is no dedicated
/// I²C pair to respect and any free pin does.
///
/// GP18 and GP19 are chosen over GP6/GP7 — also free — because they are **edge
/// castellations rather than back pads**, and this board is hand-soldered.
///
/// ## Panels are not interchangeable parts
///
/// The production part is a 0.96" 128×64 **SSD1315**, a 4-pin I²C module on
/// `0x3C`. Two others were tried first and are worth knowing about only when
/// swapping one in. Two pins either way, so this file does not care.
///
/// The 0.91" 128×32 modules are 4-pin I²C parts: `GND · VCC · SCL · SDA` and
/// nothing else. The 1.3" 128×64 Adafruit breakout has **eight** pins — `Data ·
/// Clk · SA0 · Rst · CS · 3v3 · Vin · Gnd` — because it speaks SPI as well.
/// `Data` and `Clk` are the same two wires; the rest are mode and address
/// selection.
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

/// The encoder's `A` line. **GPIO4** on both boards.
///
/// A strapping pin, and harmless as one — see *Which pins are usable* above.
#[macro_export]
macro_rules! encoder_a_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO4
    };
}

/// The encoder's `B` line. **GPIO5** on both boards. See [`encoder_a_pin!`].
#[macro_export]
macro_rules! encoder_b_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO5
    };
}

/// See [`SWITCH_PIN`].
pub const ENCODER_A_PIN: &str = "GPIO4";
/// See [`SWITCH_PIN`].
pub const ENCODER_B_PIN: &str = "GPIO5";

/// Which way round `A` and `B` are, relative to the menu.
///
/// Turning clockwise should move *down* the menu and *forward* through the
/// pages. If it goes the other way, flip this — or swap the two wires, which
/// is the same fix.
pub const ENCODER_REVERSED: bool = false;

/// Whether the encoder also rests at `00`, giving two detents per electrical
/// cycle. If one detent moves the cursor two places, or every other detent
/// does nothing, this is the wrong way round. See `encoder.rs`.
pub const ENCODER_HALF_STEP: bool = false;
