//! Motor drive, over a DRV8833 H-bridge.
//!
//! The feeder task talks to [`MotorDriver`] rather than to pins, so the same
//! task runs against the real bridge or against a stand-in while the hardware
//! is on order.

use esp_hal::gpio::{Level, Output, OutputConfig, OutputPin};
use log::info;

/// Something that can drive the feed motor one way and stop it.
///
/// There is no reverse. The hub only ever turns one way, so reversing would
/// serve no purpose and could jam the mechanism against its own geometry.
pub trait MotorDriver {
    /// Turn the hub in the feeding direction.
    fn run_forward(&mut self);

    /// Stop hard, shorting the windings.
    ///
    /// Braking rather than coasting is what makes the hub park in the same
    /// position on every feed.
    fn brake(&mut self);
}

/// The real thing.
///
/// | IN1 | IN2 | |
/// |---|---|---|
/// | 1 | 0 | forward |
/// | 0 | 0 | coast |
/// | 1 | 1 | brake |
///
/// Written from the truth table in `CLAUDE.md` and **not yet tested against a
/// board**: the breakout is still on order. Treat the first run as bring-up,
/// and check the direction before bolting it to a feeder.
pub struct Drv8833<'d> {
    in1: Output<'d>,
    in2: Output<'d>,
    /// Held high for the life of the driver.
    _sleep: Output<'d>,
}

impl<'d> Drv8833<'d> {
    /// `nsleep` is the breakout's `nSLEEP` / `ULT` pin. It is not pulled up on
    /// the board, so it must be driven high or the bridge stays asleep and the
    /// motor never moves however the inputs are set.
    pub fn new(
        in1: impl OutputPin + 'd,
        in2: impl OutputPin + 'd,
        nsleep: impl OutputPin + 'd,
    ) -> Self {
        let config = OutputConfig::default();
        Self {
            // Start braked rather than coasting, so a reset cannot leave the
            // hub free to drift off its parked position.
            in1: Output::new(in1, Level::High, config),
            in2: Output::new(in2, Level::High, config),
            _sleep: Output::new(nsleep, Level::High, config),
        }
    }
}

impl MotorDriver for Drv8833<'_> {
    fn run_forward(&mut self) {
        self.in1.set_high();
        self.in2.set_low();
    }

    fn brake(&mut self) {
        self.in1.set_high();
        self.in2.set_high();
    }
}

/// A motor that only writes to the log.
///
/// Lets the whole feeding loop be exercised on a bench with nothing but the
/// switch wired: requests, alignment, spacing rejection, counting and the jam
/// timeout all behave exactly as they will with a real bridge, because the
/// decisions live in `feeder::Feeder` rather than here.
///
/// Logs transitions only, so a loop that brakes on every iteration is visible
/// rather than drowned in repeats.
#[derive(Default)]
pub struct LogMotor {
    running: bool,
}

impl LogMotor {
    pub const fn new() -> Self {
        Self { running: false }
    }
}

impl MotorDriver for LogMotor {
    fn run_forward(&mut self) {
        if !self.running {
            self.running = true;
            info!("motor: forward");
        }
    }

    fn brake(&mut self) {
        if self.running {
            self.running = false;
            info!("motor: brake");
        }
    }
}
