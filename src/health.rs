//! How the unit is doing, as one snapshot: what the LED shows, and anything
//! else that wants the same facts at the same instant.
//!
//! Pure, and state in ADR-0028's layers: `wiring::Bus::health` takes it,
//! `indicator.rs` decides what it means for the LED.

/// What the unit knows about itself, sampled together.
///
/// A plain snapshot rather than references to the live atomics, so a reader
/// cannot observe a value changing underneath it, and the LED's logic can be
/// tested on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Health {
    /// The BOOT button is being held towards a reset. See `reset.rs`.
    pub reset_held: bool,
    /// Setup mode: this unit has raised its own access point.
    pub setup: bool,
    /// Associated with the configured Wi-Fi network.
    pub link: bool,
    /// Connected to the MQTT broker.
    pub broker: bool,
    /// The clock has been given a *live* time, so the schedule is armed. A
    /// retained time does not count — see `schedule.rs`.
    pub armed: bool,
    pub paused: bool,
    pub feeding: bool,
    pub jammed: bool,
}
