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
