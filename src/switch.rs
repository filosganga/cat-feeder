//! Debounced click stream from the hub microswitch.
//!
//! One click is one portion. The hub gives four per revolution.

use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, InputConfig, InputPin, Pull};

/// How long the contacts must hold their new state to count as settled.
///
/// At 8 rpm a real click arrives every ~1900 ms, so 30 ms is far below
/// anything the mechanism can produce and rejects contact bounce comfortably.
pub const DEBOUNCE: Duration = Duration::from_millis(30);

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
    /// Resolves on the next debounced press.
    async fn next_click(&mut self);

    /// Whether the switch is pressed right now.
    ///
    /// The feeder needs this to decide whether the align phase has to run.
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
    async fn next_click(&mut self) {
        loop {
            self.input.wait_for_falling_edge().await;

            // Let the contacts settle, then check the line actually stayed
            // down. Bounce on release also produces falling edges, and this is
            // what rejects them: after the window they read high again.
            Timer::after(DEBOUNCE).await;

            if self.input.is_low() {
                // Reported one debounce window after the true edge. That lag
                // is constant, so braking on a click still parks the hub in
                // the same place every time: ~1.4° of overshoot at 8 rpm.
                return;
            }
        }
    }

    fn is_pressed(&self) -> bool {
        self.input.is_low()
    }
}
