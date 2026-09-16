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

use crate::portions::{Added, Pending, clicks_for};

/// How long the switch contacts must settle, in milliseconds.
///
/// The same figure as `switch::DEBOUNCE`, which derives its `Duration` from
/// this. It lives here rather than there because `switch.rs` is hardware-gated
/// and this module is pure, and the spacing floor below has to be expressed
/// against it — a minimum spacing near the debounce would start rejecting real
/// clicks instead of bounce.
pub const DEBOUNCE_MS: u64 = 30;

/// The two timings that depend on how fast a particular mechanism turns.
///
/// **Derived from one measurement, not configured separately.** The minimum
/// click spacing and the jam timeout are not independent facts: both are
/// consequences of how long a detent takes. Measuring the interval and
/// computing these keeps the property that matters — that the spacing
/// threshold sits in the empty middle between contact bounce and a real
/// detent — true at any speed, rather than needing to be re-reasoned per unit.
///
/// The ratios are not invented. At the 1900 ms interval of the two matching
/// feeders they give 760 ms and 4750 ms, against the 800 ms and 5000 ms that
/// were picked by hand and work. They are what the working mechanism implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// Edges closer together than this cannot be real while the motor drives.
    ///
    /// Just after the motor starts the hub is resting on an edge, and a
    /// fraction of a turn can bounce the switch into a spurious falling edge at
    /// zero rotation. This rejects that.
    ///
    /// It is only valid because the motor guarantees a floor on how fast
    /// detents can arrive, which is exactly why it lives here and not in the
    /// switch's click stream: a hub turned by hand, or a bench button, can
    /// legitimately produce faster edges.
    pub min_click_spacing_ms: u64,
    /// No edge within this long while turning means the mechanism is stuck.
    pub jam_timeout_ms: u64,
}

impl Timings {
    /// Never let the spacing get near the debounce, or it starts rejecting real
    /// clicks rather than bounce.
    const MIN_SPACING_MS: u64 = 4 * DEBOUNCE_MS;

    /// A jam budget short enough to be useless would report a jam on a
    /// mechanism that is merely slow to start.
    const MIN_JAM_MS: u64 = 1_000;

    /// Two fifths of a detent, and two and a half of them.
    pub const fn from_detent(detent_ms: u16) -> Self {
        let detent = detent_ms as u64;

        let spacing = detent * 2 / 5;
        let jam = detent * 5 / 2;

        Self {
            min_click_spacing_ms: if spacing < Self::MIN_SPACING_MS {
                Self::MIN_SPACING_MS
            } else {
                spacing
            },
            jam_timeout_ms: if jam < Self::MIN_JAM_MS {
                Self::MIN_JAM_MS
            } else {
                jam
            },
        }
    }
}

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
    timings: Timings,
    /// How much a click of *this* mechanism dispenses, as a percentage.
    portion_scale_pct: u16,
}

impl Feeder {
    /// Both parameters come from this unit's record in flash, so one binary can
    /// drive three mechanisms that are not the same model.
    pub const fn new(timings: Timings, portion_scale_pct: u16) -> Self {
        Self {
            phase: Phase::Idle,
            pending: Pending::new(),
            since_ms: 0,
            jammed: false,
            timings,
            portion_scale_pct,
        }
    }

    /// Queues portions. Never replaces the outstanding total.
    ///
    /// Safe to call at any time, including mid-feed: that is what makes three
    /// fast button presses mean three portions.
    ///
    /// **This is where portions become clicks**, and the only place. Every
    /// producer — `mqtt`, `schedule`, the outside button — speaks portions,
    /// because that is the contract Home Assistant shares across all three
    /// units; from here down everything counts clicks. Scaling at each producer
    /// instead would be three chances to forget, with a failure nobody could
    /// see without weighing the food.
    ///
    /// It also puts [`MAX_CLICKS`](crate::portions::MAX_CLICKS) on the clicks
    /// side, which is where a cap protecting the hopper belongs: what empties a
    /// hopper is clicks, not intentions.
    pub fn request(&mut self, portions: u8) -> Added {
        self.pending
            .add(clicks_for(portions, self.portion_scale_pct))
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
        if now_ms.saturating_sub(self.since_ms) < self.timings.min_click_spacing_ms {
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
                let remaining = self.timings.jam_timeout_ms.saturating_sub(elapsed);
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
    use crate::portions::SCALE_UNCHANGED;

    /// The two matching feeders: 1900 ms between detents, one click one portion.
    ///
    /// Every test below that does not care about calibration uses this, so the
    /// numbers in their assertions stay the ones the mechanism on the bench
    /// actually produces.
    fn feeder() -> Feeder {
        Feeder::new(Timings::from_detent(REFERENCE_DETENT_MS), SCALE_UNCHANGED)
    }

    const REFERENCE_DETENT_MS: u16 = 1_900;

    /// The jam budget [`feeder`] runs with, named so the assertions below read
    /// against the mechanism rather than against a number that only happens to
    /// be right for it.
    const JAM: u64 = Timings::from_detent(REFERENCE_DETENT_MS).jam_timeout_ms;

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
        let feeder = feeder();
        assert_eq!(feeder.action(0), Action::Idle);
    }

    #[test]
    fn a_request_of_zero_does_not_start_the_motor() {
        let mut feeder = feeder();
        feeder.request(0);
        feeder.start(0, true);
        assert_eq!(feeder.action(0), Action::Idle);
    }

    // --- the four cases CLAUDE.md names -------------------------------------

    #[test]
    fn starting_pressed_counts_the_first_edge_as_a_portion() {
        // The hub normally rests on a detent, where the last feed braked.
        let mut feeder = feeder();
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
        let mut feeder = feeder();
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
        let mut feeder = feeder();
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
        let mut feeder = feeder();
        feeder.request(1);
        feeder.start(0, true);

        assert_eq!(
            feeder.action(0),
            Action::Turning {
                jam_timeout_ms: JAM as u32
            }
        );

        feeder.on_timeout();
        assert!(feeder.is_jammed());
        assert_eq!(feeder.action(JAM), Action::Idle);
    }

    // --- the jam budget -----------------------------------------------------

    #[test]
    fn the_jam_budget_counts_down_rather_than_restarting() {
        let mut feeder = feeder();
        feeder.request(2);
        feeder.start(0, true);

        assert_eq!(
            feeder.action(2_000),
            Action::Turning {
                jam_timeout_ms: (JAM - 2_000) as u32
            }
        );
    }

    #[test]
    fn repeated_bounce_cannot_postpone_jam_detection() {
        // The mechanism is stuck but the contact keeps chattering. If a
        // rejected edge reset the window, this feeder would never report a jam
        // and would sit there with the motor energised.
        let mut feeder = feeder();
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
                jam_timeout_ms: (JAM - 4_000) as u32
            },
            "the budget must still be shrinking despite the bounce"
        );

        assert_eq!(
            feeder.action(JAM),
            Action::Turning { jam_timeout_ms: 0 },
            "budget exhausted, so the wrapper times out immediately"
        );
    }

    #[test]
    fn a_counted_click_refreshes_the_jam_budget() {
        let mut feeder = feeder();
        feeder.request(2);
        feeder.start(0, true);

        feeder.on_click(1_900);
        assert_eq!(
            feeder.action(1_900),
            Action::Turning {
                jam_timeout_ms: JAM as u32
            }
        );
    }

    #[test]
    fn a_jam_discards_everything_outstanding() {
        let mut feeder = feeder();
        feeder.request(5);
        feeder.start(0, true);
        feeder.on_click(1_900);

        feeder.on_timeout();
        assert_eq!(feeder.pending(), 0);
        assert!(feeder.is_jammed());
    }

    #[test]
    fn the_jam_flag_clears_once_the_mechanism_moves_again() {
        let mut feeder = feeder();
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
        let mut feeder = feeder();
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
        let mut feeder = feeder();
        feeder.request(1);
        feeder.start(0, true);

        // Home Assistant publishes another feed while the motor is running.
        feeder.request(2);
        assert_eq!(feeder.pending(), 3);

        assert_eq!(turn_until_idle(&mut feeder, 0), 3);
    }

    #[test]
    fn three_fast_presses_mean_three_portions() {
        let mut feeder = feeder();
        feeder.request(1);
        feeder.request(1);
        feeder.request(1);
        feeder.start(0, true);

        assert_eq!(turn_until_idle(&mut feeder, 0), 3);
    }

    #[test]
    fn requests_beyond_the_cap_are_dropped_not_wrapped() {
        let mut feeder = feeder();
        for _ in 0..50 {
            feeder.request(10);
        }
        assert_eq!(feeder.pending(), crate::portions::MAX_CLICKS);
    }

    // --- stray input --------------------------------------------------------

    #[test]
    fn turning_the_hub_by_hand_while_idle_dispenses_nothing() {
        let mut feeder = feeder();
        assert_eq!(feeder.on_click(1_000), ClickOutcome::NotTurning);
        assert_eq!(feeder.pending(), 0);
        assert_eq!(feeder.action(1_000), Action::Idle);
    }

    #[test]
    fn alignment_also_rejects_startup_bounce() {
        let mut feeder = feeder();
        feeder.request(1);
        feeder.start(0, false);

        assert_eq!(feeder.on_click(50), ClickOutcome::TooSoon);
        assert_eq!(feeder.on_click(1_000), ClickOutcome::Aligned);
        assert_eq!(feeder.pending(), 1);
    }

    // --- per-unit calibration -----------------------------------------------

    #[test]
    fn the_reference_mechanism_reproduces_the_hand_picked_constants() {
        // 800 ms and 5 s were chosen by hand and work on the two matching
        // feeders. If the derivation did not land next to them, the ratios
        // would be invented rather than inferred.
        let t = Timings::from_detent(REFERENCE_DETENT_MS);
        assert_eq!(t.min_click_spacing_ms, 760);
        assert_eq!(t.jam_timeout_ms, 4_750);
    }

    #[test]
    fn a_faster_mechanism_gets_a_tighter_window() {
        let fast = Timings::from_detent(600);
        let slow = Timings::from_detent(3_000);

        assert!(fast.min_click_spacing_ms < slow.min_click_spacing_ms);
        assert!(fast.jam_timeout_ms < slow.jam_timeout_ms);
    }

    #[test]
    fn the_spacing_never_approaches_the_debounce() {
        // The threshold has to sit in the empty middle between contact bounce
        // and a real detent. Too close to the debounce and it starts rejecting
        // real clicks, which looks exactly like a mechanism that has stalled.
        for detent_ms in 1..=5_000u16 {
            let t = Timings::from_detent(detent_ms);
            assert!(
                t.min_click_spacing_ms >= 4 * DEBOUNCE_MS,
                "{detent_ms} ms gives a {} ms window",
                t.min_click_spacing_ms
            );
            assert!(t.jam_timeout_ms >= 1_000);
        }
    }

    #[test]
    fn the_spacing_always_leaves_room_for_a_real_detent() {
        // The other half: the window must never be so wide that a genuine
        // detent is rejected as bounce. Below the floor the clamp dominates,
        // which is why that range is excluded rather than asserted over.
        for detent_ms in 300..=5_000u16 {
            let t = Timings::from_detent(detent_ms);
            assert!(
                t.min_click_spacing_ms < detent_ms as u64,
                "{detent_ms} ms would reject its own clicks"
            );
        }
    }

    #[test]
    fn a_scaled_unit_turns_further_for_the_same_request() {
        // The whole point: feeder/schedule is shared, so the same "2 portions"
        // reaches every unit and each turns as far as its own mechanism needs.
        let mut small_clicks = Feeder::new(Timings::from_detent(REFERENCE_DETENT_MS), 150);
        small_clicks.request(2);
        assert_eq!(small_clicks.pending(), 3);

        let mut big_clicks = Feeder::new(Timings::from_detent(REFERENCE_DETENT_MS), 50);
        big_clicks.request(2);
        assert_eq!(big_clicks.pending(), 1);
    }

    #[test]
    fn a_scaled_unit_still_feeds_when_asked_for_one() {
        // clicks_for guarantees this, but it is worth pinning at the level
        // someone would actually debug: a unit that never turns.
        let mut tiny = Feeder::new(Timings::from_detent(REFERENCE_DETENT_MS), 10);
        tiny.request(1);
        assert_eq!(tiny.pending(), 1, "a meal was scaled away to nothing");
    }

    #[test]
    fn the_cap_applies_to_clicks_not_to_what_was_asked_for() {
        // MAX_CLICKS protects the hopper, and what empties a hopper is clicks.
        let mut feeder = Feeder::new(Timings::from_detent(REFERENCE_DETENT_MS), 200);
        feeder.request(u8::MAX);
        assert_eq!(feeder.pending(), crate::portions::MAX_CLICKS);
    }
}
