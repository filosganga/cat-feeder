# Mechanical timing is one measured number per unit, stored in flash

Feeders differ (one known variant takes ~1.9 s between detents, another
~5.3 s), but one binary must drive them all, so timing cannot be a compile-time
constant or a `cfg.toml` value. It lives in the unit's flash record, set by
`provision.sh --detent-ms`, the knob, or the admin page's *Run calibration*.

Only the **detent interval** is measured; everything else is derived:

| Constant | Rule | Why this ratio |
|---|---|---|
| minimum click spacing | interval × 0.4 | sits in the empty middle between contact bounce (ms) and a real detent |
| jam timeout | interval × 2.5 | tolerates a slower, loaded mechanism |

Both have floors expressed against `DEBOUNCE_MS`. `feeder.rs` tests that the
spacing stays clear of the debounce and below a real detent for every interval
from 1 ms to 10 s. The ratios reproduce the hand-picked values that worked on
the first mechanism (800 ms / 5000 ms at ~1.9 s).

**Calibrate with a full hopper.** A loaded mechanism turns slower. Calibrating
on the slow case keeps the spacing safe at the fast case (it only misfires if
loaded/empty exceeds 2.5); calibrating empty makes the jam budget too tight when
the hopper is full — a false jam on refill day.

A new calibration is applied by the feeder task at its next idle moment, never
mid-turn, and saving it does not restart the unit.
