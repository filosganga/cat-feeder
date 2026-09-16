//! The pending-portions counter.
//!
//! Pure logic, no hardware and no clock, so it is fully testable on the host.
//! This is the part of the feeder that decides *how many* portions are owed;
//! `feeder.rs` will own the motor and the switch and decide *when*.
//!
//! The rules it encodes come from `CLAUDE.md`:
//!
//! - Requests accumulate rather than replace, so three button presses during
//!   one feed mean three portions.
//! - The total is capped, so a stuck automation cannot empty the hopper.
//! - A jam discards whatever is outstanding, because feeding a queue into a
//!   jammed mechanism is worse than losing a meal.

/// Most portions that can ever be outstanding at once.
///
/// A cap rather than a guess: Home Assistant's quality-of-service level 1 may
/// deliver the same publish twice, and an automation stuck in a loop would
/// otherwise keep the motor running until the hopper is empty.
pub const MAX_PORTIONS: u8 = 10;

/// A [`portion_scale`](clicks_for) that changes nothing.
pub const SCALE_UNCHANGED: u16 = 100;

/// How many clicks this unit must turn to dispense `portions`.
///
/// **Specified and tested, not yet wired up.** The scale comes from the
/// per-unit record in flash, which is roadmap step 9; see *Per-unit portion
/// size* in `CLAUDE.md`.
///
/// ## Why a scale exists at all
///
/// `feeder/schedule` is a single retained topic shared by every unit, so a slot
/// saying `portions: 2` means the same request reaches all three. The feeders
/// are not all the same model, and a click on one mechanism need not dispense
/// the same amount of food as a click on another. Without a per-unit scale, one
/// feeder quietly over- or under-feeds forever, and nothing in the system can
/// see it.
///
/// So **portions are the contract and clicks are the mechanism**: everything
/// arriving from outside — the Home Assistant button, `feeder/<id>/feed`,
/// `feeder/all/feed`, every schedule slot — speaks portions, and this is the
/// one place they become clicks.
///
/// ## The rule that matters
///
/// A request for one or more portions **never becomes zero clicks**, however
/// small the scale. Rounding a meal away would be a feeder that silently stops
/// feeding, which is the failure this whole project is built to avoid. Zero in
/// still gives zero out, because `feed 0` is a documented no-op rather than a
/// meal.
///
/// Rounding is to nearest, and per request — no remainder is carried between
/// meals. Carrying one would make the same slot dispense two clicks some days
/// and one on others, which is impossible to read on a console and interacts
/// badly with the never-double-feed rules. The cost is that small portion counts
/// can only approximate a scale: at 133%, a one-portion meal is one click, not
/// 1.33. If that matters for a unit, the honest fix is a schedule with larger
/// counts, not a cleverer rounding rule.
pub fn clicks_for(portions: u8, scale_pct: u16) -> u8 {
    if portions == 0 {
        return 0;
    }

    // u32 throughout: 255 portions at a 400% scale overflows both u8 and u16.
    // The `+ 50` is round-to-nearest rather than truncation, which is what makes
    // 3 portions at 133% come out as 4 clicks instead of 3.
    let scaled = (portions as u32 * scale_pct as u32 + 50) / 100;

    // Never zero, never wrapped. The upper clamp only bites on absurd scales,
    // and `Pending::add` caps the queue at MAX_PORTIONS anyway.
    scaled.clamp(1, u8::MAX as u32) as u8
}

/// Portions owed but not yet dispensed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    count: u8,
}

/// What [`Pending::add`] did with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    /// The whole request was taken.
    All,
    /// The cap was reached and this many portions were dropped.
    Clamped { dropped: u8 },
    /// The request was for zero portions, which is a no-op rather than an error.
    Nothing,
}

impl Pending {
    pub const fn new() -> Self {
        Self { count: 0 }
    }

    /// Adds a request to the outstanding total, saturating at [`MAX_PORTIONS`].
    ///
    /// Never replaces the current total. That is the whole point: a request
    /// arriving mid-feed must add to what is already owed.
    pub fn add(&mut self, portions: u8) -> Added {
        if portions == 0 {
            return Added::Nothing;
        }

        let wanted = self.count.saturating_add(portions);
        let capped = wanted.min(MAX_PORTIONS);
        let dropped = wanted - capped;
        self.count = capped;

        if dropped > 0 {
            Added::Clamped { dropped }
        } else {
            Added::All
        }
    }

    /// Consumes one portion. Returns false when nothing was owed.
    pub fn take_one(&mut self) -> bool {
        if self.count == 0 {
            false
        } else {
            self.count -= 1;
            true
        }
    }

    /// Drops everything outstanding. Used on a jam.
    pub fn clear(&mut self) {
        self.count = 0;
    }

    pub fn count(&self) -> u8 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unscaled_feeder_is_unchanged() {
        for portions in 0..=20 {
            assert_eq!(clicks_for(portions, SCALE_UNCHANGED), portions);
        }
    }

    #[test]
    fn a_bigger_click_needs_fewer_of_them() {
        // The worked example: a unit whose click dispenses half again as much.
        // Three portions should come out as two clicks, not three.
        assert_eq!(clicks_for(3, 67), 2);
        assert_eq!(clicks_for(6, 67), 4);
    }

    #[test]
    fn a_smaller_click_needs_more_of_them() {
        assert_eq!(clicks_for(3, 133), 4);
        assert_eq!(clicks_for(6, 133), 8);
    }

    #[test]
    fn a_meal_is_never_rounded_away() {
        // The rule this function exists to guarantee. A feeder that silently
        // dispenses nothing is the failure mode the whole project avoids, and
        // it would be invisible: Home Assistant sees the request succeed.
        for scale in 1..=SCALE_UNCHANGED {
            assert!(
                clicks_for(1, scale) >= 1,
                "one portion at {scale}% became no clicks at all"
            );
        }
    }

    #[test]
    fn zero_portions_still_means_zero_clicks() {
        // `feed 0` is a documented no-op. The never-zero rule must not turn it
        // into a meal nobody asked for.
        for scale in [1, 50, SCALE_UNCHANGED, 250, u16::MAX] {
            assert_eq!(clicks_for(0, scale), 0);
        }
    }

    #[test]
    fn scaling_rounds_to_nearest_not_down() {
        // Truncation would make every scaled feeder under-feed systematically.
        assert_eq!(clicks_for(1, 150), 2, "1.5 rounds up");
        assert_eq!(clicks_for(1, 149), 1, "1.49 rounds down");
        assert_eq!(clicks_for(2, 125), 3, "2.5 rounds up");
    }

    #[test]
    fn an_absurd_scale_cannot_wrap_or_overflow() {
        // 255 portions at 400% is well past both u8 and u16.
        assert_eq!(clicks_for(u8::MAX, 400), u8::MAX);
        assert_eq!(clicks_for(u8::MAX, u16::MAX), u8::MAX);
    }

    #[test]
    fn scaling_carries_nothing_between_meals() {
        // Stateless on purpose. A carried remainder would make the same slot
        // give two clicks on some days and one on others, which cannot be read
        // off a console and fights the never-double-feed guard.
        assert_eq!(clicks_for(1, 133), 1);
        assert_eq!(clicks_for(1, 133), 1);
        assert_eq!(clicks_for(1, 133), 1);
    }

    #[test]
    fn starts_empty() {
        let pending = Pending::new();
        assert!(pending.is_empty());
        assert_eq!(pending.count(), 0);
    }

    #[test]
    fn requests_accumulate_rather_than_replace() {
        // Three button presses during one feed must mean three portions.
        let mut pending = Pending::new();
        assert_eq!(pending.add(1), Added::All);
        assert_eq!(pending.add(1), Added::All);
        assert_eq!(pending.add(1), Added::All);
        assert_eq!(pending.count(), 3);
    }

    #[test]
    fn a_manual_feed_and_a_scheduled_one_add_up() {
        let mut pending = Pending::new();
        pending.add(2); // Home Assistant
        pending.add(1); // the scheduler
        assert_eq!(pending.count(), 3);
    }

    #[test]
    fn zero_portions_is_a_no_op() {
        let mut pending = Pending::new();
        pending.add(2);
        assert_eq!(pending.add(0), Added::Nothing);
        assert_eq!(pending.count(), 2);
    }

    #[test]
    fn clamps_at_the_cap_and_reports_what_was_dropped() {
        let mut pending = Pending::new();
        assert_eq!(pending.add(MAX_PORTIONS), Added::All);
        assert_eq!(pending.add(3), Added::Clamped { dropped: 3 });
        assert_eq!(pending.count(), MAX_PORTIONS);
    }

    #[test]
    fn a_single_oversized_request_is_clamped_not_wrapped() {
        // u8 arithmetic must saturate, never wrap around to a small number.
        let mut pending = Pending::new();
        assert_eq!(
            pending.add(u8::MAX),
            Added::Clamped {
                dropped: u8::MAX - MAX_PORTIONS
            }
        );
        assert_eq!(pending.count(), MAX_PORTIONS);
    }

    #[test]
    fn repeated_oversized_requests_do_not_wrap() {
        let mut pending = Pending::new();
        for _ in 0..10 {
            pending.add(u8::MAX);
        }
        assert_eq!(pending.count(), MAX_PORTIONS);
    }

    #[test]
    fn draining_consumes_one_at_a_time() {
        let mut pending = Pending::new();
        pending.add(2);

        assert!(pending.take_one());
        assert_eq!(pending.count(), 1);
        assert!(pending.take_one());
        assert!(pending.is_empty());
    }

    #[test]
    fn taking_from_empty_reports_failure_rather_than_underflowing() {
        let mut pending = Pending::new();
        assert!(!pending.take_one());
        assert_eq!(pending.count(), 0);
    }

    #[test]
    fn a_jam_discards_everything_outstanding() {
        // Resuming a queue into a jammed mechanism is worse than a missed meal.
        let mut pending = Pending::new();
        pending.add(5);
        pending.clear();
        assert!(pending.is_empty());
    }

    #[test]
    fn work_arriving_mid_drain_is_not_lost() {
        let mut pending = Pending::new();
        pending.add(1);
        assert!(pending.take_one());

        // The motor is still turning when two more requests land.
        pending.add(2);
        assert_eq!(pending.count(), 2);
    }
}
