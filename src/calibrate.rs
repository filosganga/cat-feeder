//! Measuring the detent interval on the mechanism itself.
//!
//! Pure logic. The feeder task turns the motor and hands click timestamps
//! here; this decides what they measured. Host-tested, because a calibration
//! that saves a wrong figure makes every later jam timeout and spacing floor
//! wrong with it.
//!
//! ## What a run is
//!
//! [`DETENTS`] clicks in one continuous turn, which dispenses that many
//! portions — the measurement is only worth taking **with a full hopper**, the
//! slowest the mechanism ever runs, so a bowl belongs under it.
//!
//! The first click is the reference and is not measured *to*: the time from
//! the motor starting to it includes spin-up, and may include aligning onto a
//! detent. The [`DETENTS`]` - 1` gaps after it are clean detents.
//!
//! ## Which figure is kept
//!
//! **The slowest gap, rounded up to 10 ms.** ADR-0006
//! (`docs/adr/0006-calibration-is-one-measured-number-per-unit.md`) says to calibrate on the slowest case: the jam timeout
//! (`× 2.5`) fails if sized on a fast run, while the spacing floor (`× 0.4`) is
//! safe either way. The slowest of a few gaps is that rule applied inside one
//! run. Rounding up keeps it on the safe side too.
//!
//! A run whose gaps disagree by more than [`MAX_SPREAD_PCT`] is refused rather
//! than averaged: a gap twice the others is a missed click, a gap half the
//! others a doubled one, and neither is a speed.
//!
//! ## What a unit is calibrated by
//!
//! Two figures, [`Calibration`]: the detent interval a run measures, and the
//! portion scale, which is set by hand. Both are in the flash record.

use heapless::Vec;

/// A detent interval for a mechanism nobody has measured yet.
///
/// The figure from the two matching feeders. It is a starting point, not a
/// default worth relying on — the whole reason this is in the record is that
/// the third unit is a different brand.
pub const DEFAULT_DETENT_MS: u16 = 1_900;

/// Below this, the derived minimum click spacing would collide with the 30 ms
/// debounce in `switch.rs` and start rejecting real clicks.
pub const MIN_DETENT_MS: u16 = 200;

/// The slowest detent anything will store, by hand, by a calibration run or
/// through `mkrecord`. The matching pair of feeders take about
/// 2 s; the third, a cheaper mechanism, measured 5.3 s at 4.7 V, and turns
/// slower still on batteries or with a full hopper. Twice that, with room over.
pub const MAX_DETENT_MS: u16 = 10_000;

/// Clicks in a run, and so portions dispensed by it. Five gives four clean
/// gaps in about ten seconds on a 2 s mechanism, twenty-five on the slow one.
pub const DETENTS: usize = 5;

/// How long a run waits for any click before calling the mechanism stuck.
///
/// Fixed rather than taken from the current calibration, which is the very
/// figure being measured and may be badly wrong. Half as long again as the
/// slowest detent that can be stored, so a run on a mechanism that slow
/// reports it as [`Failure::TooSlow`] rather than as a jam.
pub const JAM_MS: u64 = MAX_DETENT_MS as u64 * 3 / 2;

/// The slowest gap may be at most this percentage of the fastest.
pub const MAX_SPREAD_PCT: u64 = 125;

/// What a good run measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measurement {
    /// The figure to save: the slowest gap, rounded up to 10 ms.
    pub detent_ms: u16,
    pub fastest_ms: u64,
    pub slowest_ms: u64,
}

/// Why a run cannot be saved. A measurement above [`MAX_DETENT_MS`] is
/// refused rather than clamped, because clamping would save a figure nobody
/// measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// No click within [`JAM_MS`].
    Jammed,
    /// The gaps disagree by more than [`MAX_SPREAD_PCT`]: a click missed or
    /// doubled.
    Inconsistent { fastest_ms: u64, slowest_ms: u64 },
    /// Faster than the record accepts. The switch is bouncing, not a detent.
    TooFast { slowest_ms: u64 },
    /// Slower than the record can store; the mechanism is nearly stalled.
    TooSlow { slowest_ms: u64 },
}

/// Where the latest run is, for a control to show. Published by the feeder
/// task, which is the only one that knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    /// No run since power-on.
    #[default]
    None,
    /// Turning; this many clicks counted of [`DETENTS`].
    Running { clicks: u8 },
    /// The last run ended like this. Nothing is saved until someone saves it.
    Finished(Result<Measurement, Failure>),
}

/// One of the figures a unit is calibrated by. Both live in the flash record
/// beside the credentials, and both take effect without a restart: the feeder
/// task picks the new figures up at its next idle moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `portions::clicks_for`'s scale, in percent.
    PortionScale,
    /// The detent interval every mechanical timing is derived from, in ms.
    Detent,
}

impl Field {
    /// The smallest, largest, and one step's worth of change: all a control
    /// may store, and the steps it should offer.
    ///
    /// Detent's floor is the record's own, so nothing can store a value the
    /// record would then refuse to read. The ceilings are well past any
    /// mechanism measured, and within what `feeder.rs`'s tests cover.
    pub const fn range(self) -> (u16, u16, u16) {
        match self {
            Field::PortionScale => (25, 300, 5),
            Field::Detent => (MIN_DETENT_MS, MAX_DETENT_MS, 10),
        }
    }
}

/// The calibration in force: what the record holds, and what the feeder task
/// counts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibration {
    pub portion_scale_pct: u16,
    pub detent_ms: u16,
}

impl Calibration {
    pub fn get(self, field: Field) -> u16 {
        match field {
            Field::PortionScale => self.portion_scale_pct,
            Field::Detent => self.detent_ms,
        }
    }
}

/// One calibration run, click by click.
#[derive(Debug, Default)]
pub struct Run {
    clicks: Vec<u64, DETENTS>,
}

impl Run {
    pub const fn new() -> Self {
        Self { clicks: Vec::new() }
    }

    /// A click arrived at `now_ms`. Returns how many have arrived so far.
    /// Clicks past [`DETENTS`] are ignored; the caller stops at [`Run::done`].
    pub fn on_click(&mut self, now_ms: u64) -> usize {
        let _ = self.clicks.push(now_ms);
        self.clicks.len()
    }

    pub fn done(&self) -> bool {
        self.clicks.len() == DETENTS
    }

    /// What the run measured, once [`Run::done`]; a run cut short is a jam.
    pub fn result(&self) -> Result<Measurement, Failure> {
        if !self.done() {
            return Err(Failure::Jammed);
        }

        let gaps = self.clicks.windows(2).map(|w| w[1].saturating_sub(w[0]));
        let (fastest_ms, slowest_ms) =
            gaps.fold((u64::MAX, 0), |(lo, hi), g| (lo.min(g), hi.max(g)));

        if slowest_ms < MIN_DETENT_MS as u64 {
            return Err(Failure::TooFast { slowest_ms });
        }
        if slowest_ms * 100 > fastest_ms * MAX_SPREAD_PCT {
            return Err(Failure::Inconsistent {
                fastest_ms,
                slowest_ms,
            });
        }

        let detent_ms = slowest_ms.div_ceil(10) * 10;
        if detent_ms > MAX_DETENT_MS as u64 {
            return Err(Failure::TooSlow { slowest_ms });
        }

        Ok(Measurement {
            detent_ms: detent_ms as u16,
            fastest_ms,
            slowest_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run with clicks at these times.
    fn run(at: &[u64]) -> Run {
        let mut r = Run::new();
        for &t in at {
            r.on_click(t);
        }
        r
    }

    /// Clicks `gaps` apart after a first one at 3_000 ms — the spin-up.
    fn spaced(gaps: &[u64]) -> Run {
        let mut t = 3_000;
        let mut at = alloc::vec![t];
        for g in gaps {
            t += g;
            at.push(t);
        }
        run(&at)
    }

    #[test]
    fn the_slowest_gap_is_kept_rounded_up() {
        let r = spaced(&[2_038, 2_061, 2_044, 2_050]);
        assert_eq!(
            r.result(),
            Ok(Measurement {
                detent_ms: 2_070,
                fastest_ms: 2_038,
                slowest_ms: 2_061,
            })
        );
    }

    /// The spin-up before the first click is not a detent, however long.
    #[test]
    fn the_time_to_the_first_click_is_not_measured() {
        let r = run(&[9_000, 11_000, 13_000, 15_000, 17_000]);
        assert_eq!(r.result().map(|m| m.detent_ms), Ok(2_000));
    }

    #[test]
    fn an_exact_multiple_of_ten_is_not_rounded_further() {
        assert_eq!(spaced(&[1_900; 4]).result().map(|m| m.detent_ms), Ok(1_900));
    }

    /// A gap twice the others is a click the switch missed.
    #[test]
    fn a_missed_click_is_refused_not_averaged() {
        assert!(matches!(
            spaced(&[2_000, 4_000, 2_000, 2_000]).result(),
            Err(Failure::Inconsistent { .. })
        ));
    }

    /// A gap far shorter than the others is a click counted twice.
    #[test]
    fn a_doubled_click_is_refused() {
        assert!(matches!(
            spaced(&[2_000, 1_000, 1_000, 2_000]).result(),
            Err(Failure::Inconsistent { .. })
        ));
    }

    #[test]
    fn a_spread_within_the_limit_is_accepted() {
        // 2_500 is exactly 125% of 2_000.
        assert!(spaced(&[2_000, 2_500, 2_200, 2_100]).result().is_ok());
        assert!(spaced(&[2_000, 2_501, 2_200, 2_100]).result().is_err());
    }

    #[test]
    fn a_run_cut_short_is_a_jam() {
        assert_eq!(run(&[1_000, 3_000, 5_000]).result(), Err(Failure::Jammed));
        assert_eq!(Run::new().result(), Err(Failure::Jammed));
    }

    #[test]
    fn bounce_speeds_are_refused() {
        assert!(matches!(
            spaced(&[50, 50, 50, 50]).result(),
            Err(Failure::TooFast { .. })
        ));
    }

    #[test]
    fn a_nearly_stalled_mechanism_is_refused_not_clamped() {
        assert!(matches!(
            spaced(&[MAX_DETENT_MS as u64 + 200; 4]).result(),
            Err(Failure::TooSlow { .. })
        ));
    }

    /// The third feeder's mechanism, as measured on USB.
    #[test]
    fn the_slow_mechanism_is_stored() {
        assert_eq!(spaced(&[5_297; 4]).result().map(|m| m.detent_ms), Ok(5_300));
    }

    /// A run on the slowest storable mechanism must see its clicks before it
    /// gives up waiting for them.
    #[test]
    fn the_run_waits_longer_than_the_slowest_detent() {
        assert!(JAM_MS > MAX_DETENT_MS as u64);
    }

    /// Whatever a run saves, a control could have stored by hand too.
    #[test]
    fn the_limits_agree_with_the_detent_range() {
        let (min, max, _) = Field::Detent.range();
        assert_eq!(MIN_DETENT_MS, min);
        assert_eq!(MAX_DETENT_MS, max);
    }

    #[test]
    fn clicks_past_the_run_are_ignored() {
        let mut r = spaced(&[2_000; 4]);
        assert!(r.done());
        assert_eq!(r.on_click(99_999), DETENTS);
        assert_eq!(r.result().map(|m| m.detent_ms), Ok(2_000));
    }
}
