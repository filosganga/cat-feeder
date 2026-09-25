//! The rotary encoder's `A`/`B` pair: levels in, detents out.
//!
//! Pure logic. The task samples both pins and hands the levels here; this
//! decides whether the knob moved a detent and which way. Host-tested, because
//! a knob that skips or doubles steps is maddening to diagnose by turning it.
//!
//! ## Why this needs no debounce, and no capacitors
//!
//! `A` and `B` are two switches a quarter-cycle apart, so a turn walks the
//! pair through a Gray sequence — exactly one of them changes at a time:
//!
//! ```text
//!   11 → 01 → 00 → 10 → 11     one detent one way
//!   11 → 10 → 00 → 01 → 11     one detent the other
//! ```
//!
//! Every valid transition is worth +1 or −1, and a detent is four of them.
//! Contact bounce only ever flips *one* line back and forth, which is a +1
//! followed by a −1: it adds a step and immediately takes it away again. So
//! the running total survives bounce by construction, and a detent is reported
//! only when the pair is back at rest with most of a cycle behind it.
//!
//! Both lines changing at once is not a transition an encoder can make. It
//! means a sample was missed, and the direction is unknowable, so it counts as
//! nothing rather than as a guess.
//!
//! ## Rest is `11`
//!
//! The common goes to ground and each line has the chip's pull-up, so both
//! contacts open — the resting state of an EC11 between detents — reads high.
//! Some encoders also rest at `00` (half-step parts, two detents per electrical
//! cycle); [`Decoder::new`] takes that as a parameter rather than a guess.

/// Transitions needed, in one direction, before arriving at rest counts as a
/// detent. A full cycle is four; accepting two tolerates one missed sample in
/// each half, and bounce can never reach it because it always cancels.
const DETENT_THRESHOLD: i8 = 2;

/// The two-bit state of the pair, `A` as the high bit.
fn state(a: bool, b: bool) -> u8 {
    ((a as u8) << 1) | b as u8
}

/// +1, −1, or 0 for a transition between two states.
///
/// The sequence `00 → 01 → 11 → 10 → 00` is +1. Anything that is not a single
/// step along it — no change, or both lines at once — is 0.
fn delta(from: u8, to: u8) -> i8 {
    const FORWARD: [u8; 4] = [0b01, 0b11, 0b00, 0b10]; // successor of 00, 01, 10, 11
    if FORWARD[from as usize] == to {
        1
    } else if FORWARD[to as usize] == from {
        -1
    } else {
        0
    }
}

/// Tracks the pair and reports whole detents.
#[derive(Debug, Clone)]
pub struct Decoder {
    last: u8,
    /// Transitions since the pair last rested.
    travelled: i8,
    /// `A` and `B` wired the other way round from the direction the menu
    /// expects. Swapping two wires and flipping this are the same fix.
    reversed: bool,
    /// Also rest at `00`, for half-step encoders.
    half_step: bool,
}

impl Decoder {
    /// `a` and `b` are the levels read at start-up, `true` for high.
    pub fn new(a: bool, b: bool, reversed: bool, half_step: bool) -> Self {
        Self {
            last: state(a, b),
            travelled: 0,
            reversed,
            half_step,
        }
    }

    /// A fresh sample of both lines. Returns +1 or −1 for a detent, else 0.
    ///
    /// Call it as often as the lines can change; calling it with an unchanged
    /// pair is free and returns 0.
    pub fn update(&mut self, a: bool, b: bool) -> i8 {
        let now = state(a, b);
        if now == self.last {
            return 0;
        }

        self.travelled = self.travelled.saturating_add(delta(self.last, now));
        self.last = now;

        let at_rest = now == 0b11 || (self.half_step && now == 0b00);
        if !at_rest {
            return 0;
        }

        let travelled = core::mem::take(&mut self.travelled);
        let step = if travelled >= DETENT_THRESHOLD {
            1
        } else if travelled <= -DETENT_THRESHOLD {
            -1
        } else {
            0
        };

        if self.reversed { -step } else { step }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REST: (bool, bool) = (true, true);

    /// One detent in the +1 direction, as the pair of levels it passes through.
    const CW: [(bool, bool); 4] = [(true, false), (false, false), (false, true), (true, true)];
    /// The same detent the other way.
    const CCW: [(bool, bool); 4] = [(false, true), (false, false), (true, false), (true, true)];

    fn feed(decoder: &mut Decoder, levels: &[(bool, bool)]) -> alloc::vec::Vec<i8> {
        levels
            .iter()
            .map(|&(a, b)| decoder.update(a, b))
            .filter(|&s| s != 0)
            .collect()
    }

    fn at_rest() -> Decoder {
        Decoder::new(REST.0, REST.1, false, false)
    }

    #[test]
    fn one_detent_is_one_step_and_only_on_arriving_at_rest() {
        let mut d = at_rest();
        let steps: alloc::vec::Vec<i8> = CW.iter().map(|&(a, b)| d.update(a, b)).collect();

        assert_eq!(steps, [0, 0, 0, 1]);
    }

    #[test]
    fn the_other_way_is_the_other_sign() {
        let mut d = at_rest();
        assert_eq!(feed(&mut d, &CCW), [-1]);
    }

    #[test]
    fn reversed_swaps_the_sign_and_nothing_else() {
        let mut d = Decoder::new(true, true, true, false);
        assert_eq!(feed(&mut d, &CW), [-1]);
        assert_eq!(feed(&mut d, &CCW), [1]);
    }

    #[test]
    fn several_detents_are_several_steps() {
        let mut d = at_rest();
        let turn: alloc::vec::Vec<_> = CW.iter().cycle().take(4 * 5).copied().collect();
        assert_eq!(feed(&mut d, &turn), [1, 1, 1, 1, 1]);
    }

    #[test]
    fn a_change_of_direction_mid_detent_goes_nowhere() {
        // Half a detent one way, then back: the knob was nudged and let go.
        let mut d = at_rest();
        let nudge = [CW[0], CW[1], CW[0], REST];
        assert_eq!(feed(&mut d, &nudge), [] as [i8; 0]);
    }

    /// The property the module docs rest on: bounce flips one line back and
    /// forth, and each flip cancels the one before it.
    #[test]
    fn bounce_on_every_edge_still_counts_one_detent() {
        let mut d = at_rest();
        let mut bouncy = alloc::vec::Vec::new();
        let mut previous = REST;
        for &next in &CW {
            for _ in 0..3 {
                bouncy.push(next);
                bouncy.push(previous);
            }
            bouncy.push(next);
            previous = next;
        }

        assert_eq!(feed(&mut d, &bouncy), [1]);
    }

    #[test]
    fn bounce_at_rest_is_not_a_step() {
        let mut d = at_rest();
        let chatter = [
            (true, false),
            REST,
            (false, true),
            REST,
            (true, false),
            REST,
        ];
        assert_eq!(feed(&mut d, &chatter), [] as [i8; 0]);
    }

    /// Both lines at once is a missed sample. Counting it either way would be a
    /// guess, so it counts as nothing — and one missed sample still leaves
    /// enough of the cycle to recognise the detent.
    #[test]
    fn a_missed_sample_is_tolerated_not_guessed() {
        let mut d = at_rest();
        // 11 → 10 → (00 missed) → 01 → 11: the jump 10 → 01 is both lines.
        let skipped = [CW[0], CW[2], CW[3]];
        assert_eq!(feed(&mut d, &skipped), [1]);
    }

    #[test]
    fn a_half_step_encoder_counts_at_both_rests() {
        let mut d = Decoder::new(true, true, false, true);
        assert_eq!(feed(&mut d, &CW), [1, 1]);
    }

    #[test]
    fn a_full_step_encoder_does_not_count_the_midpoint() {
        let mut d = at_rest();
        assert_eq!(feed(&mut d, &CW[..2]), [] as [i8; 0]);
    }

    #[test]
    fn repeated_samples_are_free() {
        let mut d = at_rest();
        for _ in 0..100 {
            assert_eq!(d.update(true, true), 0);
        }
        assert_eq!(feed(&mut d, &CW), [1]);
    }
}
