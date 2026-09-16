//! Debounced click stream from the hub microswitch.
//!
//! One click is one portion. That is the whole contract — how many clicks make
//! a revolution is not something this firmware knows or needs to.

use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, InputConfig, InputPin, Pull};

/// How long the contacts must hold their new state to count as settled.
///
/// At 8 rpm a real click arrives every ~1900 ms, so 30 ms is far below
/// anything the mechanism can produce and rejects contact bounce comfortably.
pub const DEBOUNCE: Duration = Duration::from_millis(crate::feeder::DEBOUNCE_MS);

/// How often the level is sampled.
const POLL: Duration = Duration::from_millis(5);

/// Consecutive equal samples needed to accept a new level: [`DEBOUNCE`] worth.
const STABLE_SAMPLES: u8 = 6;

/// A source of switch clicks.
///
/// This exists so the feeder logic can be exercised on the host with a fake,
/// without a board, a motor or a hub.
///
/// A `ClickSource` reports **every** real edge, however fast. The ~800 ms
/// minimum spacing that rejects motor-startup bounce deliberately does *not*
/// live here: that floor only holds while the motor is driving, so it belongs
/// to the feeder. Enforcing it here would make this stream lie about what it
/// observed, and would swallow real presses on a hand-turned hub or a bench
/// button.
#[allow(
    async_fn_in_trait,
    reason = "the executor is single-threaded, so these futures are never \
    required to be Send; adding the bound would only constrain the fakes"
)]
pub trait ClickSource {
    /// Resolves on the next debounced change of state, returning the settled
    /// level: true for pressed.
    ///
    /// Transitions rather than presses, because the resting level matters as
    /// much as the edges. The feeder needs to know whether the hub is sitting
    /// on a detent before it starts, and a task watching only falling edges
    /// cannot tell when the contact opened again.
    async fn next_transition(&mut self) -> bool;

    /// Whether the switch is pressed right now.
    fn is_pressed(&self) -> bool;
}

/// The real switch, on a GPIO with the internal pull-up enabled.
///
/// Idle reads high; closing the switch to ground reads low. A press is
/// therefore a falling edge, which is what the whole feeding state machine
/// counts.
pub struct Switch<'d> {
    input: Input<'d>,
}

impl<'d> Switch<'d> {
    /// No external resistor: the pull-up is internal and enabled here.
    pub fn new(pin: impl InputPin + 'd) -> Self {
        let config = InputConfig::default().with_pull(Pull::Up);
        Self {
            input: Input::new(pin, config),
        }
    }
}

impl ClickSource for Switch<'_> {
    /// Sampled rather than interrupt-driven.
    ///
    /// `wait_for_any_edge` would be the idiomatic choice and may well be fine;
    /// it was swapped out while chasing missing clicks that turned out to look
    /// like an intermittent connection rather than a software fault. Polling was
    /// kept because it is easier to reason about and cannot miss a transition
    /// that happens while no future is armed.
    ///
    /// The cost is negligible. The hub changes state twice per 1.9 s at 8 rpm,
    /// so a 5 ms sample is roughly 200 times faster than the signal, and the
    /// resulting 0.24 degrees of angular uncertainty is far below anything the
    /// mechanism cares about.
    ///
    /// Worth revisiting once the real hub is wired, if the idle wakeups ever
    /// matter.
    async fn next_transition(&mut self) -> bool {
        let mut settled = self.input.is_low();
        let mut candidate = settled;
        let mut stable: u8 = 0;

        loop {
            Timer::after(POLL).await;

            let level = self.input.is_low();
            if level == candidate {
                stable = stable.saturating_add(1);
            } else {
                candidate = level;
                stable = 1;
            }

            // Bounce never holds one level for a whole debounce window, so
            // requiring a run of equal samples rejects it without needing to
            // see the edges themselves.
            if candidate != settled && stable >= STABLE_SAMPLES {
                settled = candidate;
                return settled;
            }
        }
    }

    fn is_pressed(&self) -> bool {
        self.input.is_low()
    }
}
