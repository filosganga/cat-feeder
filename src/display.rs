//! What the OLED shows. Pure logic; the pixels are somebody else's problem.
//!
//! The panel that fits the original LCD window is a 0.91" 128×32, which at
//! `FONT_6X10` is **three lines of twenty-one characters**. That is the entire
//! budget, and it is why every line here is built against [`COLS`] rather than
//! formatted hopefully and truncated later.
//!
//! ## Nothing is said when nothing is wrong
//!
//! The same rule the LED follows, for the same reason: a status that is always
//! displayed is a status nobody reads. So the top line is **empty** when the
//! unit is healthy, and the screen spends all three lines on what the feeder is
//! actually for — when it last fed and when it will next.
//!
//! The difference from the LED is that a screen is read deliberately, from
//! arm's length, so when something *is* wrong it says so in words rather than
//! in a flash count. `NO BROKER` needs no decoding.
//!
//! ## One ladder, two outputs
//!
//! The top line is derived from [`Status`], which is the LED's ladder — not a
//! second one written to match. Two ladders would drift, and the failure would
//! be a screen and a light disagreeing about the same unit, which is worse than
//! either being absent. [`Status::of`] picks the winner; this module only
//! decides what that winner is called in English.
//!
//! One consequence worth stating: `Paused` is on that ladder and so appears
//! here, even though it is not a fault. A feeder left paused is the one state
//! where cats do not eat and nothing alarms, so it earns the line.

use crate::indicator::Status;
use crate::schedule::{Slot, Wall};
use heapless::String;

/// Characters per line at `FONT_6X10` on a 128-pixel-wide panel.
pub const COLS: usize = 21;

/// Lines at `FONT_6X10` on a 32-pixel-high panel: 3 × 10 px, 2 px spare.
pub const ROWS: usize = 3;

/// The address the setup form is served on. Matches `setup.rs`.
const SETUP_URL: &str = "http://192.168.4.1";

/// One rendered line, already clipped to what the panel can show.
pub type Line = String<COLS>;

/// A whole screen, ready to draw. Blank lines are blank on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screen {
    pub lines: [Line; ROWS],
}

impl Screen {
    /// The lines, for a driver to walk in order.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|l| l.as_str())
    }
}

/// The setup network's own credentials, shown only while unconfigured.
///
/// This is the display's strongest argument for existing. A unit in setup mode
/// cannot otherwise tell you the password of the network it just raised — that
/// is the whole reason for the salted derivation, `dev/ap-password.sh`, and
/// printing stickers before a unit is first powered on. On a screen it is
/// simply readable, and the sticker drops from required to backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupInfo<'a> {
    pub ssid: &'a str,
    pub password: &'a str,
}

/// When this unit last dispensed, and how much.
///
/// Portions as *requested*, never clicks, for the same reason the MQTT state
/// payload reports them that way: the schedule asked in portions and answering
/// in clicks would make the screen disagree with Home Assistant's history on a
/// unit with a portion scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fed {
    pub at: Wall,
    pub portions: u8,
}

/// Everything the screen is allowed to know.
///
/// Deliberately not a borrow of `Bus`: this module stays host-testable, and a
/// view assembled by the caller is also the only way the tests can pose states
/// that are awkward to reach on hardware.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// From the LED's ladder. See the module docs.
    pub status: Status,
    /// Present only in setup mode, where it replaces the whole screen.
    pub setup: Option<SetupInfo<'a>>,
    /// `None` until this unit feeds for the first time since booting.
    pub last_fed: Option<Fed>,
    /// The next slot due, or `None` when the schedule cannot say — no schedule,
    /// no trusted time, or paused.
    pub next: Option<Slot>,
}

/// How long the panel stays lit after a press.
///
/// Longer than the button's ten-second armed window on purpose: reading the
/// screen and arming the button are different intentions, and a screen that
/// went dark while you were still reading it would be worse than one that
/// stayed on a little too long.
pub const AWAKE_MS: u64 = 30_000;

/// Whether the panel should be lit.
///
/// **OLEDs burn in.** A feeder spends years showing the same `next 08:00` in
/// the same pixels, which is the worst case for the technology: a static image
/// on a panel that is never off. So the screen sleeps, and a press on the
/// outside button wakes it.
///
/// This costs nothing in reporting, because the screen is not the always-on
/// channel — the LED is. The division is the same one that makes them worth
/// having separately: the LED answers *is anything wrong* from the doorway, and
/// the screen answers *what exactly* when you walk over and press the button.
/// You are at the feeder either way by the time the screen matters.
///
/// Two states are exempt, and both for the same reason — the screen is the only
/// place the information exists:
///
/// - **Setup.** The SSID and password are *why* this display was fitted. A unit
///   cannot tell you them any other way, and blanking them while somebody is
///   typing into a phone would defeat the whole feature. Burn-in does not apply
///   to a state that lasts minutes and happens once.
/// - **Jammed.** Solid red on the LED says come and look; this says at what and
///   when. A jam ends when a human intervenes, so it cannot outlast attention
///   the way a fault like `NO BROKER` can — and those *do* sleep, because a
///   broker down for a week must not burn itself into the panel.
///
/// **Boot counts as a wake**, so a unit is lit for the first [`AWAKE_MS`] after
/// power-on and then sleeps like any other idle moment. Two things fall out of
/// that, and the second is the reason:
///
/// - Plugging a feeder in shows what it is doing while it does it — the walk up
///   from `NO WIFI` through `NO BROKER` to a next feeding time, which is
///   precisely the window in which something might not come up.
/// - **A dead panel stops looking like a sleeping one.** With sleep as the
///   resting state those are otherwise identical, and the only honest way to
///   tell them apart is to see the screen light of its own accord at least
///   once. It is the same argument that keeps `led_selftest` in `main.rs`,
///   answered here without a self-test to maintain.
pub fn awake(status: Status, now_ms: u64, last_press_ms: Option<u64>) -> bool {
    if matches!(status, Status::Setup | Status::Jammed) {
        return true;
    }

    // `unwrap_or(0)` is what makes boot a wake: with nothing pressed yet, the
    // window is measured from time zero rather than from nothing at all.
    //
    // `saturating_sub` rather than a comparison: `now_ms` is milliseconds since
    // boot as a u64, so it cannot wrap in any plausible life of a feeder, but a
    // press recorded fractionally ahead of a reading would otherwise underflow
    // into thirty million years of wakefulness.
    now_ms.saturating_sub(last_press_ms.unwrap_or(0)) < AWAKE_MS
}

/// Lays out one screen.
pub fn render(view: &View) -> Screen {
    // Setup mode takes the whole panel. There is nothing else worth showing:
    // the unit has no clock, no schedule and no history, and the one thing the
    // person standing in front of it needs is how to join this network.
    if let Some(setup) = view.setup {
        return Screen {
            lines: [clip(setup.ssid), clip(setup.password), clip(SETUP_URL)],
        };
    }

    Screen {
        lines: [
            banner(view.status),
            fed_line(view.last_fed),
            next_line(view),
        ],
    }
}

/// The top line: what is wrong, in words, or nothing at all.
///
/// `Setup` never reaches here — it is handled above, because it replaces the
/// screen rather than heading it.
fn banner(status: Status) -> Line {
    let text = match status {
        // Asterisks because this one wants somebody to walk over and look, and
        // it is the only state on the ladder that does.
        Status::Jammed => "** JAMMED **",
        Status::Feeding => "FEEDING",
        Status::Armed => "TAP TO FEED",
        Status::Setup => "SETUP",
        Status::NoLink => "NO WIFI",
        Status::NoBroker => "NO BROKER",
        // Names the fix rather than the symptom. "NO TIME" would read as a
        // clock fault on the unit, when what it means is that nothing is
        // publishing `feeder/time` — and until something does, this unit will
        // not feed at all.
        Status::NoTime => "WAITING FOR HA TIME",
        Status::Paused => "PAUSED",
        Status::Healthy => "",
    };

    clip(text)
}

/// `fed  08:00  x2`, or an honest admission that it has not.
fn fed_line(last: Option<Fed>) -> Line {
    let Some(fed) = last else {
        // Not a fault: a unit that has just booted has genuinely never fed, and
        // saying so is better than a blank that reads as a missing feature.
        return clip("fed  never yet");
    };

    let mut line = Line::new();
    push(&mut line, "fed  ");
    push_hhmm(&mut line, fed.at.second_of_day / 60);
    push(&mut line, "  x");
    push_u8(&mut line, fed.portions);
    line
}

/// `next 19:00  x2`, when there is an answer.
fn next_line(view: &View) -> Line {
    // Two states have an upcoming slot and will not act on it, and in both the
    // honest answer is silence rather than a time.
    //
    // `Paused`: slots that fall due are marked consumed, not deferred, so the
    // meal is not late, it is not happening.
    //
    // `NoTime`: the clock has never been handed a *live* time, so the schedule
    // is holding and **this unit will not feed at all**. `Scheduler::upcoming`
    // still answers, because it only reads the slot list — deciding whether the
    // unit will act is this module's job, not its.
    //
    // Getting this wrong would be the worst thing the screen could do: print
    // `next 19:00` on a feeder that has already decided not to, with the
    // reason sitting one line above it.
    if matches!(view.status, Status::Paused | Status::NoTime) {
        return Line::new();
    }

    let Some(slot) = view.next else {
        return Line::new();
    };

    let mut line = Line::new();
    push(&mut line, "next ");
    push_hhmm(&mut line, slot.minute_of_day as u32);
    push(&mut line, "  x");
    push_u8(&mut line, slot.portions);
    line
}

/// Truncates to the panel rather than failing.
///
/// A line too long for the screen is a layout bug, but dropping it entirely
/// would hide the very state somebody is squinting at. Showing the first
/// twenty-one characters of `NO BROKER` still says `NO BROKER`.
fn clip(text: &str) -> Line {
    let mut line = Line::new();
    push(&mut line, text);
    line
}

/// Appends what fits and silently drops the rest. See [`clip`].
fn push(line: &mut Line, text: &str) {
    for c in text.chars() {
        if line.push(c).is_err() {
            return;
        }
    }
}

/// `HH:MM`, zero-padded, from a minute of the day.
fn push_hhmm(line: &mut Line, minute_of_day: u32) {
    let minute_of_day = minute_of_day % (24 * 60);
    push_two(line, (minute_of_day / 60) as u8);
    push(line, ":");
    push_two(line, (minute_of_day % 60) as u8);
}

fn push_two(line: &mut Line, value: u8) {
    let _ = line.push((b'0' + (value / 10) % 10) as char);
    let _ = line.push((b'0' + value % 10) as char);
}

fn push_u8(line: &mut Line, mut value: u8) {
    if value == 0 {
        let _ = line.push('0');
        return;
    }

    let mut digits = [0u8; 3];
    let mut n = 0;
    while value > 0 {
        digits[n] = b'0' + value % 10;
        value /= 10;
        n += 1;
    }
    while n > 0 {
        n -= 1;
        let _ = line.push(digits[n] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Date;

    fn wall(hour: u32, minute: u32) -> Wall {
        Wall {
            date: Date {
                year: 2026,
                month: 9,
                day: 17,
            },
            second_of_day: hour * 3600 + minute * 60,
            offset_minutes: Some(120),
        }
    }

    fn view() -> View<'static> {
        View {
            status: Status::Healthy,
            setup: None,
            last_fed: None,
            next: None,
        }
    }

    /// The constraint the whole module exists to respect.
    fn assert_fits(screen: &Screen) {
        for line in screen.lines() {
            assert!(
                line.chars().count() <= COLS,
                "{line:?} is {} characters, the panel holds {COLS}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn a_healthy_unit_says_nothing_at_the_top() {
        let screen = render(&View {
            last_fed: Some(Fed {
                at: wall(8, 0),
                portions: 2,
            }),
            next: Some(Slot {
                minute_of_day: 19 * 60,
                portions: 2,
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "");
        assert_eq!(screen.lines[1], "fed  08:00  x2");
        assert_eq!(screen.lines[2], "next 19:00  x2");
        assert_fits(&screen);
    }

    #[test]
    fn a_fault_takes_the_top_line_and_leaves_the_rest() {
        let screen = render(&View {
            status: Status::NoBroker,
            last_fed: Some(Fed {
                at: wall(8, 0),
                portions: 2,
            }),
            next: Some(Slot {
                minute_of_day: 19 * 60,
                portions: 2,
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "NO BROKER");
        // The history is still worth reading while the broker is down — it is
        // the only place left that knows whether the cats have eaten.
        assert_eq!(screen.lines[1], "fed  08:00  x2");
        assert_eq!(screen.lines[2], "next 19:00  x2");
        assert_fits(&screen);
    }

    #[test]
    fn paused_drops_the_next_feed_rather_than_promising_one() {
        let screen = render(&View {
            status: Status::Paused,
            last_fed: Some(Fed {
                at: wall(8, 0),
                portions: 2,
            }),
            // Even with a slot in hand, a paused unit will not feed at it: the
            // slot is marked consumed when it falls due, never deferred.
            next: Some(Slot {
                minute_of_day: 19 * 60,
                portions: 2,
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "PAUSED");
        assert_eq!(screen.lines[2], "", "a paused unit has no next feed");
        assert_fits(&screen);
    }

    /// The same rule as paused, for the state that is easiest to get wrong:
    /// the schedule is holding, so the slot exists and will not be fed.
    #[test]
    fn no_trusted_time_promises_nothing_either() {
        let screen = render(&View {
            status: Status::NoTime,
            next: Some(Slot {
                minute_of_day: 19 * 60,
                portions: 2,
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "WAITING FOR HA TIME");
        assert_eq!(
            screen.lines[2], "",
            "a unit whose schedule is holding must not name a next feed"
        );
    }

    #[test]
    fn setup_mode_replaces_the_whole_screen() {
        let screen = render(&View {
            status: Status::Setup,
            setup: Some(SetupInfo {
                ssid: "cat-feeder-99177c",
                password: "H75T-C7VT-6FAV",
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "cat-feeder-99177c");
        assert_eq!(screen.lines[1], "H75T-C7VT-6FAV");
        assert_eq!(screen.lines[2], "http://192.168.4.1");
        assert_fits(&screen);
    }

    #[test]
    fn a_unit_that_has_never_fed_says_so() {
        let screen = render(&view());

        assert_eq!(screen.lines[1], "fed  never yet");
        assert_fits(&screen);
    }

    #[test]
    fn midnight_and_noon_are_not_confused() {
        let midnight = render(&View {
            last_fed: Some(Fed {
                at: wall(0, 5),
                portions: 1,
            }),
            ..view()
        });
        assert_eq!(midnight.lines[1], "fed  00:05  x1");

        let noon = render(&View {
            last_fed: Some(Fed {
                at: wall(12, 0),
                portions: 1,
            }),
            ..view()
        });
        assert_eq!(noon.lines[1], "fed  12:00  x1");
    }

    #[test]
    fn the_last_minute_of_the_day_is_not_the_first() {
        let screen = render(&View {
            next: Some(Slot {
                minute_of_day: 23 * 60 + 59,
                portions: 1,
            }),
            ..view()
        });

        assert_eq!(screen.lines[2], "next 23:59  x1");
    }

    /// `MAX_CLICKS` is 16, and a scaled unit can be asked for more portions
    /// than that, so two digits have to fit and line up.
    #[test]
    fn two_digit_portion_counts_fit() {
        let screen = render(&View {
            last_fed: Some(Fed {
                at: wall(8, 0),
                portions: 12,
            }),
            ..view()
        });

        assert_eq!(screen.lines[1], "fed  08:00  x12");
        assert_fits(&screen);
    }

    /// Every rung of the ladder must fit, including the longest wording. This
    /// is the test that catches a banner edited to something a shade too long.
    #[test]
    fn every_status_fits_the_panel() {
        for status in [
            Status::Jammed,
            Status::Feeding,
            Status::Armed,
            Status::Setup,
            Status::NoLink,
            Status::NoBroker,
            Status::NoTime,
            Status::Paused,
            Status::Healthy,
        ] {
            let screen = render(&View { status, ..view() });
            assert_fits(&screen);
        }
    }

    /// Only `Healthy` is allowed to say nothing, or the rule the module is
    /// built on — an empty top line means all is well — stops being true.
    #[test]
    fn only_a_healthy_unit_has_an_empty_banner() {
        for status in [
            Status::Jammed,
            Status::Feeding,
            Status::Armed,
            Status::NoLink,
            Status::NoBroker,
            Status::NoTime,
            Status::Paused,
        ] {
            let screen = render(&View { status, ..view() });
            assert!(
                !screen.lines[0].is_empty(),
                "{status:?} left the top line blank, which reads as healthy"
            );
        }
    }

    // ---- sleeping ----

    /// Boot is a wake: lit while the unit comes up, then dark like any other
    /// idle moment. Without this a dead panel and a sleeping one are the same
    /// thing to look at.
    #[test]
    fn the_panel_is_lit_at_boot_and_sleeps_afterwards() {
        assert!(awake(Status::Healthy, 0, None));
        assert!(awake(Status::Healthy, AWAKE_MS - 1, None));

        assert!(!awake(Status::Healthy, AWAKE_MS, None));
        assert!(!awake(Status::Healthy, 10 * 60 * 1_000, None));
    }

    /// The boot window covers the interesting part of a start-up — the walk
    /// from no network to a next feeding time — which on the bench has taken
    /// as long as twelve seconds to reach the broker.
    #[test]
    fn the_boot_window_outlasts_a_normal_start_up() {
        assert!(
            awake(Status::NoBroker, 12_000, None),
            "a unit still finding the broker must still be readable"
        );
    }

    #[test]
    fn a_press_lights_it_for_the_window_and_no_longer() {
        assert!(awake(Status::Healthy, 1_000, Some(1_000)));
        assert!(awake(Status::Healthy, 1_000 + AWAKE_MS - 1, Some(1_000)));
        assert!(!awake(Status::Healthy, 1_000 + AWAKE_MS, Some(1_000)));
    }

    #[test]
    fn a_second_press_starts_the_window_again() {
        let first = 1_000;
        let second = first + AWAKE_MS - 500;

        assert!(!awake(Status::Healthy, second + AWAKE_MS, Some(first)));
        assert!(awake(Status::Healthy, second + AWAKE_MS - 1, Some(second)));
    }

    /// The two states where the screen is the only place the information
    /// exists. Setup is the whole reason the panel is fitted.
    #[test]
    fn setup_and_jammed_never_sleep() {
        for status in [Status::Setup, Status::Jammed] {
            assert!(
                awake(status, 10 * 60 * 60 * 1_000, None),
                "{status:?} must stay lit with nothing pressed for ten hours"
            );
        }
    }

    /// A press must light the screen from *any* state, which is the whole
    /// point: a unit that is stuck is exactly the one worth walking over to
    /// read, and it must not be the one that refuses to answer.
    #[test]
    fn a_press_lights_it_whatever_is_wrong() {
        let long_after = AWAKE_MS * 100;

        for status in [
            Status::Healthy,
            Status::NoLink,
            Status::NoBroker,
            Status::NoTime,
            Status::Paused,
            Status::Feeding,
            Status::Armed,
        ] {
            assert!(
                awake(status, long_after, Some(long_after)),
                "{status:?} ignored a press"
            );
        }
    }

    /// A broker down for a week must not burn itself into the panel. The LED is
    /// the channel that stays on; this one is read on demand.
    #[test]
    fn an_ordinary_fault_still_sleeps() {
        for status in [Status::NoLink, Status::NoBroker, Status::NoTime] {
            assert!(
                !awake(status, AWAKE_MS * 100, Some(1_000)),
                "{status:?} kept the panel lit indefinitely"
            );
        }
    }

    /// A press stamped fractionally ahead of the reading must not underflow
    /// into effectively permanent wakefulness.
    #[test]
    fn a_press_from_the_future_does_not_wedge_it_on() {
        assert!(awake(Status::Healthy, 500, Some(1_000)));
        assert!(!awake(Status::Healthy, 1_000 + AWAKE_MS, Some(1_000)));
    }

    /// A long SSID is clipped rather than dropped, because a clipped one is
    /// still enough to pick the network out of a phone's list.
    #[test]
    fn an_overlong_setup_line_is_clipped_not_lost() {
        let screen = render(&View {
            status: Status::Setup,
            setup: Some(SetupInfo {
                ssid: "cat-feeder-with-a-very-long-name",
                password: "H75T-C7VT-6FAV",
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "cat-feeder-with-a-ver");
        assert_fits(&screen);
    }
}
