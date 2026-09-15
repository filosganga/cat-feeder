//! The feeding state machine.
//!
//! Pure logic: no motor, no switch, no clock. Time arrives as milliseconds in
//! each call, so every decision is a function of its inputs and the whole
//! machine is testable on the host without an executor.
//!
//! The async task that owns the real motor and switch is a thin wrapper. It
//! asks [`Feeder::action`] what to do, does it, and feeds the result back as an
//! event. It makes no decisions of its own.
//!
//! ```text
//!   loop {
//!       match feeder.action(now()) {
//!           Action::Idle => { motor.brake(); feeder.request(FEED.receive().await); }
//!           Action::Turning { jam_timeout_ms } => {
//!               motor.run_forward();
//!               match select(clicks.next_click(), Timer::after_millis(jam_timeout_ms)).await {
//!                   First(_)  => { feeder.on_click(now()); }
//!                   Second(_) => { motor.brake(); feeder.on_timeout(); }
//!               }
//!           }
//!       }
//!   }
//! ```
//!
//! See `CLAUDE.md` for why feeding counts edges rather than levels, and why the
//! minimum click spacing lives here rather than in `switch.rs`.

use crate::portions::{Added, Pending};

/// Edges closer together than this cannot be real while the motor is driving.
///
/// At 8 rpm a quarter turn takes about 1900 ms. Just after the motor starts the
/// hub is resting on an edge, and a fraction of a turn can bounce the switch
/// and produce a spurious falling edge at zero rotation. This rejects that.
///
/// It is only valid because the motor guarantees the 1900 ms floor, which is
/// exactly why it lives here and not in the switch's click stream: a hub turned
/// by hand, or a bench button, can legitimately produce faster edges.
pub const MIN_CLICK_SPACING_MS: u64 = 800;

/// No edge within this long while turning means the mechanism is stuck.
pub const JAM_TIMEOUT_MS: u64 = 5_000;

/// What the wrapper should be doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing owed. Brake and wait for a request.
    Idle,
    /// Run the motor forward and wait for the next click, or for this many
    /// milliseconds, whichever comes first.
    ///
    /// This is the *remaining* jam budget measured from the last counted click,
    /// not a fresh timeout. Repeated bounce therefore cannot keep pushing the
    /// jam detection further away.
    Turning { jam_timeout_ms: u32 },
}

/// What a falling edge meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickOutcome {
    /// Consumed by the align phase. The hub is now in a known position and no
    /// portion was dispensed.
    Aligned,
    /// One portion dispensed.
    Counted { remaining: u8 },
    /// Below the minimum spacing, so bounce rather than a detent. Ignored, and
    /// deliberately does not reset the spacing window.
    TooSoon,
    /// The motor is not running, so this edge means someone turned the hub by
    /// hand.
    NotTurning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Turning, but the first edge belongs to alignment rather than a portion.
    Aligning,
    Counting,
}

/// The feeding state machine.
#[derive(Debug, Clone)]
pub struct Feeder {
    phase: Phase,
    pending: Pending,
    /// When the motor started, or when the last edge was accepted. Both the
    /// spacing window and the jam budget are measured from here.
    since_ms: u64,
    jammed: bool,
}

impl Default for Feeder {
    fn default() -> Self {
        Self::new()
    }
}

impl Feeder {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            pending: Pending::new(),
            since_ms: 0,
            jammed: false,
        }
    }

    /// Queues portions. Never replaces the outstanding total.
    ///
    /// Safe to call at any time, including mid-feed: that is what makes three
    /// fast button presses mean three portions.
    pub fn request(&mut self, portions: u8) -> Added {
        self.pending.add(portions)
    }

    /// Starts turning.
    ///
    /// The wrapper calls this when [`Action::Idle`] is reported but portions
    /// are owed, passing the switch level it just read. Reading the switch is
    /// an I/O act, so the level is an input rather than something this machine
    /// can work out for itself.
    ///
    /// `switch_pressed` false means the hub is resting between detents, so the
    /// first edge is only a partial turn and must not be counted as a portion.
    pub fn start(&mut self, now_ms: u64, switch_pressed: bool) {
        if self.pending.is_empty() {
            return;
        }

        self.phase = if switch_pressed {
            Phase::Counting
        } else {
            Phase::Aligning
        };
        self.since_ms = now_ms;
    }

    /// Records a debounced falling edge.
    pub fn on_click(&mut self, now_ms: u64) -> ClickOutcome {
        if self.phase == Phase::Idle {
            return ClickOutcome::NotTurning;
        }

        // Note the window is measured from the last *accepted* event. A
        // rejected edge must not move it, or sustained bounce would ratchet the
        // window forward indefinitely.
        if now_ms.saturating_sub(self.since_ms) < MIN_CLICK_SPACING_MS {
            return ClickOutcome::TooSoon;
        }
        self.since_ms = now_ms;

        // The mechanism moved, so whatever jam we were reporting is over.
        self.jammed = false;

        if self.phase == Phase::Aligning {
            self.phase = Phase::Counting;
            return ClickOutcome::Aligned;
        }

        self.pending.take_one();
        if self.pending.is_empty() {
            self.phase = Phase::Idle;
        }

        ClickOutcome::Counted {
            remaining: self.pending.count(),
        }
    }

    /// Records that the jam budget expired with no edge.
    ///
    /// Everything outstanding is discarded: feeding a queue into a jammed
    /// mechanism is worse than losing a meal.
    pub fn on_timeout(&mut self) {
        self.phase = Phase::Idle;
        self.pending.clear();
        self.jammed = true;
    }

    /// What the wrapper should do now.
    pub fn action(&self, now_ms: u64) -> Action {
        match self.phase {
            Phase::Idle => Action::Idle,
            Phase::Aligning | Phase::Counting => {
                let elapsed = now_ms.saturating_sub(self.since_ms);
                let remaining = JAM_TIMEOUT_MS.saturating_sub(elapsed);
                Action::Turning {
                    jam_timeout_ms: remaining as u32,
                }
            }
        }
    }

    /// Portions still owed.
    pub fn pending(&self) -> u8 {
        self.pending.count()
    }

    /// Whether the last attempt to turn ended in a jam. Published in the state
    /// topic and surfaced in Home Assistant.
    pub fn is_jammed(&self) -> bool {
        self.jammed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a feed to completion by supplying clicks one full turn apart,
    /// returning how many portions were counted.
    fn turn_until_idle(feeder: &mut Feeder, start_ms: u64) -> u8 {
        let mut now = start_ms;
        let mut counted = 0;
        while feeder.action(now) != Action::Idle {
            now += 1_900;
            if let ClickOutcome::Counted { .. } = feeder.on_click(now) {
                counted += 1;
            }
        }
        counted
    }

    #[test]
    fn idle_until_something_is_requested() {
        let feeder = Feeder::new();
        assert_eq!(feeder.action(0), Action::Idle);
    }

    #[test]
    fn a_request_of_zero_does_not_start_the_motor() {
        let mut feeder = Feeder::new();
        feeder.request(0);
        feeder.start(0, true);
        assert_eq!(feeder.action(0), Action::Idle);
    }

    // --- the four cases CLAUDE.md names -------------------------------------

    #[test]
    fn starting_pressed_counts_the_first_edge_as_a_portion() {
        // The hub normally rests on a detent, where the last feed braked.
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, true);

        assert_eq!(
            feeder.on_click(1_900),
            ClickOutcome::Counted { remaining: 0 }
        );
        assert_eq!(feeder.action(1_900), Action::Idle);
    }

    #[test]
    fn starting_free_spends_the_first_edge_on_alignment() {
        // Someone turned the hub by hand, so the first edge is a partial turn.
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, false);

        assert_eq!(feeder.on_click(1_000), ClickOutcome::Aligned);
        assert_eq!(feeder.pending(), 1, "alignment must not spend a portion");

        assert_eq!(
            feeder.on_click(2_900),
            ClickOutcome::Counted { remaining: 0 }
        );
    }

    #[test]
    fn bounce_at_motor_start_is_not_a_portion() {
        // The motor nudges the hub off the detent it is resting on and the
        // contact chatters. Zero rotation, so it cannot be a portion.
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, true);

        assert_eq!(feeder.on_click(40), ClickOutcome::TooSoon);
        assert_eq!(feeder.pending(), 1);

        assert_eq!(
            feeder.on_click(1_900),
            ClickOutcome::Counted { remaining: 0 }
        );
    }

    #[test]
    fn no_clicks_at_all_is_a_jam() {
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, true);

        assert_eq!(
            feeder.action(0),
            Action::Turning {
                jam_timeout_ms: 5_000
            }
        );

        feeder.on_timeout();
        assert!(feeder.is_jammed());
        assert_eq!(feeder.action(5_000), Action::Idle);
    }

    // --- the jam budget -----------------------------------------------------

    #[test]
    fn the_jam_budget_counts_down_rather_than_restarting() {
        let mut feeder = Feeder::new();
        feeder.request(2);
        feeder.start(0, true);

        assert_eq!(
            feeder.action(2_000),
            Action::Turning {
                jam_timeout_ms: 3_000
            }
        );
    }

    #[test]
    fn repeated_bounce_cannot_postpone_jam_detection() {
        // The mechanism is stuck but the contact keeps chattering. If a
        // rejected edge reset the window, this feeder would never report a jam
        // and would sit there with the motor energised.
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, true);

        // Contact chatter is milliseconds apart, not tenths of a second.
        let mut now = 0;
        for _ in 0..20 {
            now += 5;
            assert_eq!(feeder.on_click(now), ClickOutcome::TooSoon);
        }
        assert_eq!(now, 100);

        // The budget is measured from the motor start, not from the last
        // bounce, so it has kept shrinking the whole time.
        assert_eq!(
            feeder.action(4_000),
            Action::Turning {
                jam_timeout_ms: 1_000
            },
            "the budget must still be shrinking despite the bounce"
        );

        assert_eq!(
            feeder.action(5_000),
            Action::Turning { jam_timeout_ms: 0 },
            "budget exhausted, so the wrapper times out immediately"
        );
    }

    #[test]
    fn a_counted_click_refreshes_the_jam_budget() {
        let mut feeder = Feeder::new();
        feeder.request(2);
        feeder.start(0, true);

        feeder.on_click(1_900);
        assert_eq!(
            feeder.action(1_900),
            Action::Turning {
                jam_timeout_ms: 5_000
            }
        );
    }

    #[test]
    fn a_jam_discards_everything_outstanding() {
        let mut feeder = Feeder::new();
        feeder.request(5);
        feeder.start(0, true);
        feeder.on_click(1_900);

        feeder.on_timeout();
        assert_eq!(feeder.pending(), 0);
        assert!(feeder.is_jammed());
    }

    #[test]
    fn the_jam_flag_clears_once_the_mechanism_moves_again() {
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, true);
        feeder.on_timeout();
        assert!(feeder.is_jammed());

        feeder.request(1);
        feeder.start(10_000, true);
        feeder.on_click(11_900);
        assert!(!feeder.is_jammed());
    }

    // --- accumulation -------------------------------------------------------

    #[test]
    fn two_portions_are_one_continuous_turn() {
        // The motor must not stop between portions, so the action stays
        // Turning until the last click.
        let mut feeder = Feeder::new();
        feeder.request(2);
        feeder.start(0, true);

        assert_eq!(
            feeder.on_click(1_900),
            ClickOutcome::Counted { remaining: 1 }
        );
        assert!(
            matches!(feeder.action(1_900), Action::Turning { .. }),
            "braking here would make two portions two separate starts"
        );

        assert_eq!(
            feeder.on_click(3_800),
            ClickOutcome::Counted { remaining: 0 }
        );
        assert_eq!(feeder.action(3_800), Action::Idle);
    }

    #[test]
    fn a_request_arriving_mid_turn_extends_the_same_run() {
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, true);

        // Home Assistant publishes another feed while the motor is running.
        feeder.request(2);
        assert_eq!(feeder.pending(), 3);

        assert_eq!(turn_until_idle(&mut feeder, 0), 3);
    }

    #[test]
    fn three_fast_presses_mean_three_portions() {
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.request(1);
        feeder.request(1);
        feeder.start(0, true);

        assert_eq!(turn_until_idle(&mut feeder, 0), 3);
    }

    #[test]
    fn requests_beyond_the_cap_are_dropped_not_wrapped() {
        let mut feeder = Feeder::new();
        for _ in 0..50 {
            feeder.request(10);
        }
        assert_eq!(feeder.pending(), crate::portions::MAX_PORTIONS);
    }

    // --- stray input --------------------------------------------------------

    #[test]
    fn turning_the_hub_by_hand_while_idle_dispenses_nothing() {
        let mut feeder = Feeder::new();
        assert_eq!(feeder.on_click(1_000), ClickOutcome::NotTurning);
        assert_eq!(feeder.pending(), 0);
        assert_eq!(feeder.action(1_000), Action::Idle);
    }

    #[test]
    fn alignment_also_rejects_startup_bounce() {
        let mut feeder = Feeder::new();
        feeder.request(1);
        feeder.start(0, false);

        assert_eq!(feeder.on_click(50), ClickOutcome::TooSoon);
        assert_eq!(feeder.on_click(1_000), ClickOutcome::Aligned);
        assert_eq!(feeder.pending(), 1);
    }
}
