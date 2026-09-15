//! Pin map.
//!
//! Every pin number lives here and nowhere else, so bringing up a second board
//! is a change to this file alone. Roadmap step 6 adds a `board-zero` variant
//! behind a Cargo feature; until then this is the DEV-KIT map.
//!
//! Pins are handed out by macro rather than by function because esp-hal's
//! peripheral singletons are moved, not borrowed, so a plain accessor taking
//! `&mut Peripherals` cannot give one away.

/// The hub microswitch.
///
/// GPIO11 on the DEV-KIT's J1 header, third pin in from 5V. It is not a
/// strapping pin and has no alternate function. Wired to GND through the
/// switch, with the internal pull-up enabled, so a press is a falling edge.
///
/// See `CLAUDE.md` for the wiring, including the warning about the ground pin
/// sitting next to 5V.
#[macro_export]
macro_rules! switch_pin {
    ($peripherals:expr) => {
        $peripherals.GPIO11
    };
}
