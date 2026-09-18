//! What the OLED shows. Pure logic; the pixels are somebody else's problem.
//!
//! The smaller of the two panels is a 0.91" 128×32, which at `FONT_6X10` is
//! **three lines of twenty-one characters**. That is the budget this module
//! lays out against, and it is why every line here is built against [`COLS`]
//! rather than formatted hopefully and truncated later.
//!
//! The 1.3" 128×64 under `panel-128x64` is **not wider**. Both are 128 pixels
//! across, so [`COLS`] is 21 either way and the big panel buys rows alone —
//! six instead of three. Rows are therefore a floor and columns a ceiling.
//!
//! Which matters because the ceiling is enforced by truncation and nothing
//! else: [`Line`] is a `String<COLS>`, so `push` drops the overflow in silence.
//! Nothing logs, and the obvious test does not catch it: `assert_fits` below
//! measures rendered lines, which cannot exceed [`COLS`] by construction, so it
//! confirms the type rather than the layout. What catches a truncated line is
//! asserting its exact expected text, or reading a value back out of it the way
//! `printed_seconds` does.
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

use crate::button::ARM_HOLD_MS;
use crate::indicator::Status;
use crate::schedule::{Slot, Wall};
use heapless::String;

/// Characters per line at `FONT_6X10` on a 128-pixel-wide panel.
pub const COLS: usize = 21;

/// Lines at `FONT_6X10` on a 32-pixel-high panel: 3 × 10 px, 2 px spare.
pub const ROWS: usize = 3;

/// The address the setup form is served on.
///
/// From `provisioning.rs`, which owns the setup network's identity, rather than
/// retyped here: `setup.rs` binds its socket to the same four octets, and a
/// screen confidently showing an address nothing answers on would be worse than
/// no screen at all.
const SETUP_URL: &str = crate::provisioning::AP_URL;

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
    /// Whether the outside button is armed, straight from
    /// [`Health::button_armed`](crate::wiring::Health).
    ///
    /// Carried separately because [`Status`] cannot express it during a jam:
    /// `Status::of` puts `Jammed` above `Armed`, deliberately, so that the LED
    /// keeps warning while somebody has their hands in the mechanism. The panel
    /// is the one surface that can show both at once, and a jam is exactly when
    /// both matter — see [`render`].
    ///
    /// Named for `Health`'s field rather than shortened to `armed`, which in
    /// that struct means the *schedule* is armed and is a different fact.
    pub button_armed: bool,
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

    // A jam takes the middle line for an instruction, because it is the one
    // state the panel can do something about.
    //
    // A jammed feeder recovers by being asked to feed again: nothing in
    // `feeder::Feeder` gates on the flag, and `on_click` clears it as soon as
    // the mechanism moves. The outside button already sends that request —
    // hold to arm, tap to feed — so the recovery gesture is built and always
    // was. What it lacked was any sign that it had landed, because
    // `Status::of` reports `Jammed` over `Armed` and the LED therefore stays
    // solid red through the whole hold. Without this line the only honest
    // reading of the panel is that the button is dead while jammed, which is
    // how a jam turns into a power cycle.
    //
    // The LED keeps its red: that ordering guards fingers in the mechanism and
    // is not this module's to overturn. The panel says what to do instead,
    // which it can afford because a jam is one of the two states `awake`
    // exempts from sleeping, so the line is still there whenever somebody
    // walks over to look.
    //
    // `next` is dropped rather than squeezed in, for the reason `next_line`
    // already drops it while paused or untrusted: a unit that will not reach
    // its next slot unaided should not print a time implying it will.
    if view.status == Status::Jammed {
        return Screen {
            lines: [
                banner(view.status),
                if view.button_armed {
                    clip("TAP TO RETRY")
                } else {
                    arm_hint()
                },
                fed_line(view.last_fed),
            ],
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

/// `HOLD 2s TO ARM`, with the 2 taken from [`ARM_HOLD_MS`] rather than typed.
///
/// A duration written into a string is this repo's own recurring bug — three
/// copies of the old 800 ms spacing, three of the 5 s jam timeout — and this
/// would be the worst place yet for it, because it is the line somebody reads
/// off a jammed feeder when a meal did not happen. Being told to hold for a
/// time that no longer arms anything reads as a dead button.
///
/// Delegates to [`arm_hint_for`] so the rule can be tested across durations
/// instead of only at whichever value the constant happens to hold. A test
/// that can only see one value cannot tell a derivation from a literal.
fn arm_hint() -> Line {
    arm_hint_for(ARM_HOLD_MS)
}

/// The hint for an arbitrary hold, in whole seconds, **rounded up**.
///
/// Rounding up is the whole point and is not interchangeable with rounding to
/// nearest. This line is an *instruction*, so the two directions fail very
/// differently: holding for longer than the printed time always arms, while
/// holding for exactly the printed time may not. Flooring 2 500 ms to `2s`
/// would print an instruction that does not work, which is the failure this
/// function exists to prevent rather than a rounding detail.
///
/// It also disposes of the sub-second case for free: 500 ms prints `1s` rather
/// than a `0s` nobody can act on.
///
/// The seconds count saturates at `u8::MAX` because [`push_u8`] takes one. A
/// hold of over four minutes is absurd, but a silent wrap would print a small
/// number — the unsafe direction again, and for the same reason.
fn arm_hint_for(hold_ms: u64) -> Line {
    let seconds = hold_ms.div_ceil(1_000).min(u8::MAX as u64) as u8;

    let mut line = Line::new();
    push(&mut line, "HOLD ");
    push_u8(&mut line, seconds);
    push(&mut line, "s TO ARM");
    line
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
            button_armed: false,
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

    // --- a jam says how to get out of it -------------------------------------

    #[test]
    fn a_jam_offers_the_gesture_that_recovers_it() {
        let screen = render(&View {
            status: Status::Jammed,
            last_fed: Some(Fed {
                at: wall(8, 0),
                portions: 2,
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], "** JAMMED **");
        assert_eq!(screen.lines[1], "HOLD 2s TO ARM");
        assert_eq!(screen.lines[2], "fed  08:00  x2");
        assert_fits(&screen);
    }

    #[test]
    fn an_armed_jam_says_the_tap_will_land() {
        let screen = render(&View {
            status: Status::Jammed,
            button_armed: true,
            ..view()
        });

        assert_eq!(screen.lines[0], "** JAMMED **");
        assert_eq!(screen.lines[1], "TAP TO RETRY");
        assert_fits(&screen);
    }

    /// The whole point of carrying `button_armed` beside `status`.
    ///
    /// `Status::of` reports `Jammed` over `Armed`, so a screen driven by the
    /// status alone cannot tell these two apart — and they are the two the
    /// person standing at a jammed feeder is trying to distinguish.
    #[test]
    fn arming_changes_the_jam_screen_although_the_status_does_not() {
        let locked = render(&View {
            status: Status::Jammed,
            ..view()
        });
        let armed = render(&View {
            status: Status::Jammed,
            button_armed: true,
            ..view()
        });

        assert_eq!(locked.lines[0], armed.lines[0], "both still shout JAMMED");
        assert_ne!(locked.lines[1], armed.lines[1]);
    }

    /// A jammed unit will not reach its next slot without someone intervening,
    /// so printing one would promise a meal that is not coming — the same rule
    /// `next_line` already applies to `Paused` and `NoTime`.
    #[test]
    fn a_jam_does_not_promise_the_next_meal() {
        let screen = render(&View {
            status: Status::Jammed,
            next: Some(Slot {
                minute_of_day: 19 * 60,
                portions: 2,
            }),
            ..view()
        });

        for line in screen.lines() {
            assert!(!line.contains("19:00"), "{line:?} promises the next slot");
        }
    }

    /// The hint is the only thing arming changes; the warning stays put.
    ///
    /// Not a claim about food — this module cannot make one — only that a
    /// screen never stops shouting about a jam merely because somebody armed
    /// the button. The LED cannot say both, which is why this one must.
    #[test]
    fn arming_never_removes_the_jam_warning() {
        for button_armed in [false, true] {
            let screen = render(&View {
                status: Status::Jammed,
                button_armed,
                ..view()
            });

            assert_eq!(screen.lines[0], "** JAMMED **");
            assert_fits(&screen);
        }
    }

    /// The hint follows [`ARM_HOLD_MS`] rather than a typed-in `2`.
    ///
    /// Exercised through [`arm_hint_for`] across durations, because a test that
    /// only ever sees the one value the constant currently holds cannot tell a
    /// derivation from a literal — `clip("HOLD 2s TO ARM")` would satisfy any
    /// assertion made solely about `ARM_HOLD_MS == 2_000`.
    #[test]
    fn the_arm_hint_never_asks_for_less_than_it_takes() {
        // An instruction may overstate a hold and must never understate one:
        // holding longer than the printed time always arms, holding for
        // exactly a floored figure need not. 2_500 ms is the case that made
        // this explicit — flooring prints `2s`, and 2 s would arm nothing.
        for hold_ms in [1, 500, 999, 1_000, 1_001, 2_000, 2_500, 2_999, 3_000, 10_000] {
            let line = arm_hint_for(hold_ms);
            let seconds = printed_seconds(&line) as u64;

            assert!(
                seconds * 1_000 >= hold_ms,
                "{line:?} asks for less than the {hold_ms} ms it takes"
            );
            assert!(
                seconds * 1_000 < hold_ms + 1_000,
                "{line:?} overstates a {hold_ms} ms hold by a whole second"
            );
        }
    }

    #[test]
    fn the_jam_screen_renders_that_hint_rather_than_its_own() {
        let screen = render(&View {
            status: Status::Jammed,
            ..view()
        });

        assert_eq!(screen.lines[1], arm_hint_for(ARM_HOLD_MS));
        // Spelled out as well, so a reader sees what the panel says today.
        assert_eq!(screen.lines[1], "HOLD 2s TO ARM");
    }

    /// Reads the number back out of a rendered hint.
    ///
    /// Measuring what the panel shows rather than what the arithmetic intended
    /// is also the only width check that can fail here: `Line` truncates at
    /// [`COLS`], so an over-long hint loses its `s TO ARM` tail and this
    /// stops parsing. Asserting `len() <= COLS` could never fail.
    fn printed_seconds(line: &Line) -> u8 {
        line.strip_prefix("HOLD ")
            .and_then(|rest| rest.strip_suffix("s TO ARM"))
            .expect("the hint kept its shape")
            .parse()
            .expect("the hint's number")
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
        // `AP_URL`, not the string it happens to hold: `setup.rs` binds a
        // socket built from the same octets, and a second literal here would be
        // a second place for that address to be wrong.
        assert_eq!(screen.lines[2], crate::provisioning::AP_URL);
        assert_fits(&screen);
    }

    /// The setup screen is the one whose content the *unit* decides rather than
    /// this module, so the two have to be checked against each other. An SSID a
    /// character too long is clipped, and a clipped SSID does not match the one
    /// in a phone's Wi-Fi list — which would leave the panel confidently showing
    /// a network nobody can find.
    #[test]
    fn the_derived_setup_credentials_fit_the_panel() {
        use crate::provisioning::{AP_PASSWORD_LEN, AP_SSID_LEN, AP_URL, ap_password, ap_ssid};

        assert!(AP_SSID_LEN <= COLS, "the SSID cannot fit the panel");
        assert!(AP_PASSWORD_LEN <= COLS, "the password cannot fit the panel");
        assert!(AP_URL.len() <= COLS, "the address cannot fit the panel");

        let ssid = ap_ssid("99177c");
        let password = ap_password("s3cr3t", "99177c");
        let screen = render(&View {
            status: Status::Setup,
            setup: Some(SetupInfo {
                ssid: &ssid,
                password: &password,
            }),
            ..view()
        });

        assert_eq!(screen.lines[0], ssid.as_str(), "the SSID was clipped");
        assert_eq!(
            screen.lines[1],
            password.as_str(),
            "the password was clipped"
        );
        assert_eq!(screen.lines[2], AP_URL);
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
