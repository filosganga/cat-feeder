//! The BOOT button's reset: held for five seconds, it erases the network
//! settings and the unit reboots into setup mode.
//!
//! Pure logic, sampled by `main.rs`'s `reset_task`. It is the way back into a
//! unit that cannot reach its network — the Wi-Fi password changed, the broker
//! moved — when the admin page is unreachable for the same reason. On a unit
//! with a knob the power-on gesture does the same; a headless unit has only
//! this.
//!
//! **Only the network goes.** The record is rewritten by
//! `Record::without_network` — every credential blank, the calibration kept —
//! and meals and timezone live in sectors of their own, so they are untouched.
//! Same as the power-on gesture, because the problem being fixed is the
//! network. The
//! menu's *Factory reset* is the one that forgets everything.
//!
//! Five seconds, held without a break, because it is irreversible for the
//! unit's connection and the button sits beside the reset one on the board.
//! A release restarts the count; so does a bounce, which only makes it more
//! careful. The LED shows it after half a second, so a brush of the button
//! shows nothing and a deliberate hold shows it is being counted.

/// How long the button must be held.
pub const RESET_HOLD_MS: u64 = 5_000;

/// How long before the LED starts saying so.
pub const SHOW_AFTER_MS: u64 = 500;

/// Where a hold is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Not pressed, or pressed too briefly to show.
    Idle,
    /// Held, and long enough to show on the LED.
    Counting { remaining_ms: u64 },
    /// Held the whole time: erase and reboot. Reported once.
    Reset,
}

#[derive(Debug, Default)]
pub struct HoldToReset {
    since: Option<u64>,
    fired: bool,
}

impl HoldToReset {
    pub const fn new() -> Self {
        Self {
            since: None,
            fired: false,
        }
    }

    /// One sample of the button, `true` for pressed.
    pub fn update(&mut self, now_ms: u64, pressed: bool) -> Hold {
        if !pressed {
            self.since = None;
            return Hold::Idle;
        }
        let since = *self.since.get_or_insert(now_ms);
        let held = now_ms.saturating_sub(since);
        if held >= RESET_HOLD_MS {
            if self.fired {
                return Hold::Idle;
            }
            self.fired = true;
            return Hold::Reset;
        }
        if held < SHOW_AFTER_MS {
            return Hold::Idle;
        }
        Hold::Counting {
            remaining_ms: RESET_HOLD_MS - held,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(samples: &[(u64, bool)]) -> Vec<Hold> {
        let mut h = HoldToReset::new();
        samples.iter().map(|&(t, p)| h.update(t, p)).collect()
    }

    #[test]
    fn five_seconds_held_resets_once() {
        let out = run(&[
            (0, true),
            (400, true),
            (600, true),
            (4_999, true),
            (5_000, true),
            (5_050, true),
        ]);
        assert_eq!(out[0], Hold::Idle);
        assert_eq!(out[1], Hold::Idle);
        assert_eq!(
            out[2],
            Hold::Counting {
                remaining_ms: 4_400
            }
        );
        assert_eq!(out[3], Hold::Counting { remaining_ms: 1 });
        assert_eq!(out[4], Hold::Reset);
        assert_eq!(out[5], Hold::Idle);
    }

    #[test]
    fn letting_go_starts_the_count_again() {
        let out = run(&[
            (0, true),
            (4_000, true),
            (4_050, false),
            (4_100, true),
            (8_000, true),
            (9_100, true),
        ]);
        assert_eq!(out[2], Hold::Idle);
        assert!(matches!(out[4], Hold::Counting { .. }));
        assert_eq!(out[5], Hold::Reset);
    }

    #[test]
    fn a_brush_shows_nothing() {
        assert!(
            run(&[(0, true), (300, true), (400, false)])
                .iter()
                .all(|h| *h == Hold::Idle)
        );
    }
}
