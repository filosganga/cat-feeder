//! What the OLED shows. Pure logic; the pixels are somebody else's problem.
//!
//! The panel is a 0.96" 128×64, which at `FONT_6X10` is **six lines of
//! twenty-one characters**. That is the budget this module lays out against,
//! and it is why every line here is built against [`COLS`] rather than
//! formatted hopefully and truncated later.
//!
//! Six rows is new; the 0.91" 128×32 this module was first written for had
//! three, and was dropped as a fallback once the 128×64 part proved itself.
//! **The rows grew and the columns did not** — both panels are 128 pixels
//! wide, so [`COLS`] is still 21, and it is still the limit that bites.
//!
//! Which matters because the ceiling is enforced by truncation and nothing
//! else: [`Line`] is a `String<COLS>`, so `push` drops the overflow in silence.
//! Nothing logs, and the obvious test does not catch it: `assert_fits` below
//! measures rendered lines, which cannot exceed [`COLS`] by construction, so it
//! confirms the type rather than the layout. What catches a truncated line is
//! asserting its exact expected text, or reading a value back out of it the way
//! `printed_seconds` does.
//!
//! ## What is on screen
//!
//! The knob decides, through [`Mode`] from `menu.rs`:
//!
//! - **Locked**, a page: home (status, last feed, next feed), then the unit's
//!   network, its broker, and the device itself. Turning steps through them.
//! - **Unlocked**, the menu: `Feed`, pause or resume, `Lock`, with a cursor.
//!
//! The bottom row of every screen says what a hold will do from there, because
//! a hold is the one gesture nothing on the glass would otherwise suggest.
//!
//! ## Nothing is said when nothing is wrong
//!
//! The same rule the LED follows, for the same reason: a status that is always
//! displayed is a status nobody reads. So the top line of the home page is
//! **empty** when the unit is healthy, and the page spends its lines on what
//! the feeder is actually for — when it last fed and when it will next.
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
//!
//! ## No page shows a password
//!
//! The info pages show the SSID, the broker and the user, never a stored
//! password. Setup mode is not an exception so much as a different thing: the
//! password it shows is one the unit derived for a network it raised itself,
//! and showing it is the point.

use crate::button::ARM_HOLD_MS;
use crate::calibrate::{DETENTS, Failure, JAM_MS, Measurement};
use crate::indicator::Status;
use crate::menu::{Calibration, ClockEdit, ClockField, Field, Item, Mode, Page, Setting};
use crate::schedule::{Slot, Wall};
use heapless::String;

/// Characters per line at `FONT_6X10` on a 128-pixel-wide panel.
pub const COLS: usize = 21;

/// Lines at `FONT_6X10` on a 64-pixel-high panel: 6 × 10 px, 4 px spare.
pub const ROWS: usize = 6;

/// The address the setup form is served on.
///
/// From `provisioning.rs`, which owns the setup network's identity, rather than
/// retyped here: `setup.rs` binds its socket to the same four octets, and a
/// screen confidently showing an address nothing answers on would be worse than
/// no screen at all.
const SETUP_URL: &str = crate::provisioning::AP_URL;

/// The bottom row, where every screen says what a hold does from there.
const HINT_ROW: usize = ROWS - 1;

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

/// How this unit is configured, for the info pages. Fixed for a boot.
///
/// Carries no password, so no page *can* show one — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitInfo<'a> {
    pub id: &'a str,
    pub board: &'a str,
    pub version: &'a str,
    pub wifi_ssid: &'a str,
    pub mqtt_host: &'a str,
    pub mqtt_port: u16,
    pub mqtt_user: &'a str,
}

/// What the network is doing right now, for the info pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Net {
    pub link: bool,
    pub broker: bool,
    /// This unit's address, once DHCP has handed one out.
    pub ip: Option<[u8; 4]>,
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
    /// Which page, or the menu and its cursor. From `menu.rs`.
    ///
    /// Carried beside `status` rather than read out of it, because
    /// `Status::of` puts `Jammed` above `Armed` — deliberately, so the LED keeps
    /// warning while somebody has their hands in the mechanism — and the panel
    /// is the one surface that can show a jam and an open menu at once.
    pub mode: Mode,
    /// Whether the schedule is paused, for the menu's pause-or-resume label.
    ///
    /// Not read from `status` either: an open menu is `Armed` on the ladder,
    /// which hides `Paused`, and the label must not lie about which way a tap
    /// will flip it.
    pub paused: bool,
    pub net: Net,
    /// `None` only in setup mode, which has no configuration to describe.
    pub unit: Option<UnitInfo<'a>>,
    /// The calibration in force, which the knob can change without a restart
    /// — so it is carried here as a live value rather than in [`UnitInfo`].
    pub calibration: Calibration,
    /// The time now, only when it is trusted. A clock the schedule will not
    /// act on is not printed as if it were right.
    pub now: Option<Wall>,
    /// This unit holds no meals: never given a schedule, or given an empty
    /// one. See [`home`].
    pub no_meals: bool,
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
/// on a panel that is never off. So the screen sleeps, and a press or a turn
/// of the knob wakes it.
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
        return setup_screen(setup);
    }

    let mut screen = match view.mode {
        Mode::Unlocked { item } => menu_screen(view, item),
        Mode::Settings { item } => settings_screen(view, item),
        Mode::Editing { field, value } => edit_screen(view, field, value),
        Mode::ConfirmReset { erase } => reset_screen(erase),
        Mode::SettingClock(edit) => clock_screen(edit),
        Mode::ConfirmCalibrate { start } => confirm_calibrate_screen(start),
        Mode::Calibrating { clicks } => calibrating_screen(clicks),
        Mode::Calibrated(result) => calibrated_screen(view, result),
        Mode::Locked { page: Page::Home } => home(view),
        Mode::Locked { page } => info_page(view, page),
    };

    // A hold locks from anywhere. Inside an edit or a confirmation that also
    // throws the pending change away, and the hint says so in those words.
    screen.lines[HINT_ROW] = match view.mode {
        Mode::Locked { .. } => hold_hint(),
        // A hold mid-run locks the menu; the motor finishes its turn.
        Mode::Unlocked { .. } | Mode::Settings { .. } | Mode::Calibrating { .. } => {
            hold_hint_for(ARM_HOLD_MS, "TO LOCK")
        }
        Mode::Editing { .. }
        | Mode::ConfirmReset { .. }
        | Mode::SettingClock(_)
        | Mode::ConfirmCalibrate { .. }
        | Mode::Calibrated(_) => hold_hint_for(ARM_HOLD_MS, "TO CANCEL"),
    };
    screen
}

/// Two lines to type into a phone, and where to go once joined.
fn setup_screen(setup: SetupInfo) -> Screen {
    Screen {
        lines: [
            clip("JOIN THIS WI-FI"),
            clip(setup.ssid),
            clip(setup.password),
            Line::new(),
            clip("THEN BROWSE TO"),
            clip(SETUP_URL),
        ],
    }
}

/// Status, last feed, next feed.
///
/// A jam drops the next feed rather than squeezing it in, for the reason
/// `next_line` already drops it while paused or untrusted: a unit that will not
/// reach its next slot unaided should not print a time implying it will.
///
/// A jammed feeder recovers by being asked to feed again — nothing in
/// `feeder::Feeder` gates on the flag, and `on_click` clears it as soon as the
/// mechanism moves — and the way to ask is the hint row's hold, then a tap on
/// the menu's `Retry feed`. The LED stays solid red through all of it, so the
/// panel is what says the gesture landed: `** JAMMED **` stays at the top of
/// the menu, and its first item changes name.
fn home(view: &View) -> Screen {
    let mut screen = Screen::default();
    // A unit with no meals is online, connected, clock-trusted and dark: the
    // healthy look of a feeder that will never feed. So it takes the banner
    // that healthy leaves empty. Below every fault, because each of those is
    // also a reason no meal comes and is the one to fix first.
    screen.lines[0] = if view.status == Status::Healthy && view.no_meals {
        clip("NO MEALS SET")
    } else {
        banner(view.status)
    };
    screen.lines[1] = fed_line(view.last_fed);
    if view.status != Status::Jammed {
        screen.lines[2] = next_line(view);
    }
    // The time, so a clock set by hand — or one that has quietly gone wrong —
    // can be checked at the feeder. Below the meals, because they are what the
    // screen is for.
    if let Some(now) = view.now {
        push(&mut screen.lines[3], "now  ");
        push_hhmm(&mut screen.lines[3], now.second_of_day / 60);
    }
    screen
}

/// The menu, with `>` on the item a tap will run.
fn menu_screen(view: &View, cursor: Item) -> Screen {
    let mut screen = Screen::default();

    // The jam stays shouted while the menu is open, which the LED cannot do:
    // `Status::of` puts `Jammed` over `Armed` so red keeps warning, and the
    // panel is the one place that can show both. `FEEDING` likewise, so a tap
    // visibly lands.
    screen.lines[0] = match view.status {
        Status::Jammed | Status::Feeding => banner(view.status),
        _ => clip("MENU"),
    };

    for (row, item) in Item::ALL.iter().enumerate() {
        let label = match item {
            Item::Feed if view.status == Status::Jammed => "Retry feed",
            Item::Feed => "Feed one portion",
            Item::Pause if view.paused => "Resume schedule",
            Item::Pause => "Pause schedule",
            Item::Settings => "Settings",
            Item::Lock => "Lock",
        };

        let line = &mut screen.lines[1 + row];
        push(line, if *item == cursor { "> " } else { "  " });
        push(line, label);
    }

    screen
}

/// Calibration and the reset, each showing its current value.
fn settings_screen(view: &View, cursor: Setting) -> Screen {
    let mut screen = Screen::default();
    screen.lines[0] = clip("SETTINGS");

    // Six items and four rows between the title and the hint, so the list
    // scrolls: the window follows the cursor down and comes back up with it.
    const VISIBLE: usize = HINT_ROW - 1;
    let at = Setting::ALL.iter().position(|s| *s == cursor).unwrap_or(0);
    let first = at.saturating_sub(VISIBLE - 1);

    for (row, item) in Setting::ALL.iter().skip(first).take(VISIBLE).enumerate() {
        let line = &mut screen.lines[1 + row];
        push(line, if *item == cursor { "> " } else { "  " });
        match item {
            Setting::Clock => {
                push(line, "Clock   ");
                match view.now {
                    Some(now) => push_hhmm(line, now.second_of_day / 60),
                    None => push(line, "not set"),
                }
            }
            Setting::PortionScale => {
                push(line, "Portion ");
                push_field(
                    line,
                    Field::PortionScale,
                    view.calibration.portion_scale_pct,
                );
            }
            Setting::Detent => {
                push(line, "Detent  ");
                push_field(line, Field::Detent, view.calibration.detent_ms);
            }
            Setting::Calibrate => push(line, "Calibrate"),
            Setting::Reset => push(line, "Factory reset"),
            Setting::Back => push(line, "Back"),
        }
    }

    screen
}

/// One number, what it is now, and what a tap will store.
fn edit_screen(view: &View, field: Field, value: u16) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;

    lines[0] = clip(match field {
        Field::PortionScale => "PORTION SIZE",
        Field::Detent => "DETENT INTERVAL",
    });
    push(&mut lines[1], "now  ");
    push_field(&mut lines[1], field, view.calibration.get(field));
    push(&mut lines[2], "new  ");
    push_field(&mut lines[2], field, value);
    lines[4] = clip("TAP TO SAVE");
    screen
}

/// The one irreversible item, behind a second choice that starts on `Keep`.
fn reset_screen(erase: bool) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;
    lines[0] = clip("FACTORY RESET");
    lines[1] = clip("erases Wi-Fi, broker,");
    lines[2] = clip("calibration and meals");
    push(&mut lines[3], if erase { "  " } else { "> " });
    push(&mut lines[3], "Keep");
    push(&mut lines[4], if erase { "> " } else { "  " });
    push(&mut lines[4], "Erase, restart");
    screen
}

/// The date and time being set, with `^^` under the part the knob turns.
fn clock_screen(edit: ClockEdit) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;
    lines[0] = clip("SET CLOCK");

    // `2026-09-25  20:14`, and the columns each field occupies in it.
    push_u32(&mut lines[2], edit.year as u32);
    push(&mut lines[2], "-");
    push_two(&mut lines[2], edit.month);
    push(&mut lines[2], "-");
    push_two(&mut lines[2], edit.day);
    push(&mut lines[2], "  ");
    push_two(&mut lines[2], edit.hour);
    push(&mut lines[2], ":");
    push_two(&mut lines[2], edit.minute);

    let (from, width) = match edit.field {
        ClockField::Year => (0, 4),
        ClockField::Month => (5, 2),
        ClockField::Day => (8, 2),
        ClockField::Hour => (12, 2),
        ClockField::Minute => (15, 2),
    };
    for _ in 0..from {
        let _ = lines[3].push(' ');
    }
    for _ in 0..width {
        let _ = lines[3].push('^');
    }

    lines[4] = clip(if edit.field == ClockField::Minute {
        "TAP TO SET"
    } else {
        "TAP: NEXT"
    });
    screen
}

/// The calibration dispenses food, so it says how much before it starts.
fn confirm_calibrate_screen(start: bool) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;
    lines[0] = clip("CALIBRATE");
    push(&mut lines[1], "turns ");
    push_u32(&mut lines[1], DETENTS as u32);
    push(&mut lines[1], " detents and");
    push(&mut lines[2], "dispenses ");
    push_u32(&mut lines[2], DETENTS as u32);
    push(&mut lines[2], " portions");
    push(&mut lines[3], if start { "  " } else { "> " });
    push(&mut lines[3], "Keep");
    push(&mut lines[4], if start { "> " } else { "  " });
    push(&mut lines[4], "Start");
    screen
}

fn calibrating_screen(clicks: u8) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;
    lines[0] = clip("CALIBRATING");
    push(&mut lines[1], "click ");
    push_u32(&mut lines[1], clicks as u32);
    push(&mut lines[1], " of ");
    push_u32(&mut lines[1], DETENTS as u32);
    screen
}

/// What a run measured against what is in force, or why it failed, in words.
fn calibrated_screen(view: &View, result: Result<Measurement, Failure>) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;
    match result {
        Ok(m) => {
            lines[0] = clip("CALIBRATED");
            push(&mut lines[1], "new  ");
            push_field(&mut lines[1], Field::Detent, m.detent_ms);
            push(&mut lines[2], "gaps ");
            push_u32(&mut lines[2], m.fastest_ms as u32);
            push(&mut lines[2], "-");
            push_u32(&mut lines[2], m.slowest_ms as u32);
            push(&mut lines[2], "ms");
            push(&mut lines[3], "now  ");
            push_field(&mut lines[3], Field::Detent, view.calibration.detent_ms);
            lines[4] = clip("TAP TO SAVE");
        }
        Err(failure) => {
            lines[0] = clip("CALIBRATION FAILED");
            match failure {
                Failure::Jammed => {
                    push(&mut lines[1], "no click in ");
                    push_u32(&mut lines[1], (JAM_MS / 1_000) as u32);
                    push(&mut lines[1], "s");
                }
                Failure::Inconsistent {
                    fastest_ms,
                    slowest_ms,
                } => {
                    lines[1] = clip("clicks uneven");
                    push(&mut lines[2], "gaps ");
                    push_u32(&mut lines[2], fastest_ms as u32);
                    push(&mut lines[2], "-");
                    push_u32(&mut lines[2], slowest_ms as u32);
                    push(&mut lines[2], "ms");
                }
                Failure::TooFast { .. } => lines[1] = clip("too fast: bouncing?"),
                Failure::TooSlow { .. } => lines[1] = clip("too slow: stalling?"),
            }
            lines[4] = clip("TAP: BACK");
        }
    }
    screen
}

/// `x135%` or `1900ms`.
fn push_field(line: &mut Line, field: Field, value: u16) {
    match field {
        Field::PortionScale => {
            push(line, "x");
            push_u32(line, value as u32);
            push(line, "%");
        }
        Field::Detent => {
            push_u32(line, value as u32);
            push(line, "ms");
        }
    }
}

/// One of the pages a locked turn steps through.
fn info_page(view: &View, page: Page) -> Screen {
    let mut screen = Screen::default();
    let lines = &mut screen.lines;

    let Some(unit) = view.unit else {
        lines[0] = title("", page);
        lines[1] = clip("not configured");
        return screen;
    };

    match page {
        Page::Home => return home(view),
        Page::Network => {
            lines[0] = title("WI-FI", page);
            push(&mut lines[1], "ip ");
            match view.net.ip {
                Some(ip) => push_ip(&mut lines[1], ip),
                None => push(&mut lines[1], "none yet"),
            }
            lines[2] = connected(view.net.link);
            // Last, because it is the one line that may need two: an SSID can
            // be 32 bytes, and a clipped one does not match the network in a
            // router's list.
            [lines[3], lines[4]] = wrap("", unit.wifi_ssid);
        }
        Page::Broker => {
            lines[0] = title("BROKER", page);
            // `255.255.255.255:65535` is exactly twenty-one characters, so any
            // address the firmware accepts fits on one line.
            push(&mut lines[1], unit.mqtt_host);
            push(&mut lines[1], ":");
            push_u32(&mut lines[1], unit.mqtt_port as u32);
            lines[2] = connected(view.net.broker);
            [lines[3], lines[4]] = wrap("user ", unit.mqtt_user);
        }
        Page::Device => {
            // A feeder behaving oddly is either mis-measured or mis-provisioned
            // and nothing else tells them apart; this page does, at the feeder.
            lines[0] = title("DEVICE", page);
            push(&mut lines[1], "id ");
            push(&mut lines[1], unit.id);
            push(&mut lines[1], "  ");
            push(&mut lines[1], unit.board);
            push(&mut lines[2], "fw ");
            push(&mut lines[2], unit.version);
            push(&mut lines[3], "detent ");
            push_field(&mut lines[3], Field::Detent, view.calibration.detent_ms);
            push(&mut lines[4], "portion ");
            push_field(
                &mut lines[4],
                Field::PortionScale,
                view.calibration.portion_scale_pct,
            );
        }
    }

    screen
}

/// `WI-FI            2/4`: the name on the left, where it is on the right.
fn title(name: &str, page: Page) -> Line {
    let mut counter = Line::new();
    push_u32(&mut counter, page.index() as u32 + 1);
    push(&mut counter, "/");
    push_u32(&mut counter, Page::ALL.len() as u32);

    let mut line = clip(name);
    while line.len() + counter.len() < COLS {
        let _ = line.push(' ');
    }
    push(&mut line, &counter);
    line
}

/// `prefix` and `text` over two lines, so a value up to 42 characters long is
/// shown whole rather than clipped in silence. Beyond that it is clipped, which
/// no SSID reaches.
fn wrap(prefix: &str, text: &str) -> [Line; 2] {
    let mut first = clip(prefix);
    let mut rest = text.chars();
    while first.len() < COLS {
        match rest.next() {
            Some(c) => {
                let _ = first.push(c);
            }
            None => break,
        }
    }
    [first, clip(rest.as_str())]
}

fn connected(up: bool) -> Line {
    clip(if up { "connected" } else { "not connected" })
}

/// The top line: what is wrong, in words, or nothing at all.
///
/// `Setup` never reaches here — it is handled above, because it replaces the
/// screen rather than heading it.
fn banner(status: Status) -> Line {
    let text = match status {
        // Asterisks because this one wants somebody to walk over and look, and
        // it is the only state on the ladder that does.
        // The BOOT button is being held: letting go keeps everything.
        Status::Resetting => "HOLD TO ERASE WI-FI",
        Status::Jammed => "** JAMMED **",
        Status::Feeding => "FEEDING",
        Status::Armed => "MENU",
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

/// `HOLD 2s FOR MENU`, with the 2 taken from [`ARM_HOLD_MS`] rather than typed.
///
/// A duration written into a string is this repo's own recurring bug — three
/// copies of the old 800 ms spacing, three of the 5 s jam timeout — and this
/// would be the worst place yet for it, because it is the line somebody reads
/// off a jammed feeder when a meal did not happen. Being told to hold for a
/// time that no longer opens anything reads as a dead button.
///
/// Delegates to [`hold_hint_for`] so the rule can be tested across durations
/// instead of only at whichever value the constant happens to hold. A test
/// that can only see one value cannot tell a derivation from a literal.
fn hold_hint() -> Line {
    hold_hint_for(ARM_HOLD_MS, "FOR MENU")
}

/// The hint for an arbitrary hold, in whole seconds, **rounded up**.
///
/// Rounding up is the whole point and is not interchangeable with rounding to
/// nearest. This line is an *instruction*, so the two directions fail very
/// differently: holding for longer than the printed time always works, while
/// holding for exactly the printed time may not. Flooring 2 500 ms to `2s`
/// would print an instruction that does not work, which is the failure this
/// function exists to prevent rather than a rounding detail.
///
/// It also disposes of the sub-second case for free: 500 ms prints `1s` rather
/// than a `0s` nobody can act on.
fn hold_hint_for(hold_ms: u64, what: &str) -> Line {
    let seconds = hold_ms.div_ceil(1_000).min(u32::MAX as u64) as u32;

    let mut line = Line::new();
    push(&mut line, "HOLD ");
    push_u32(&mut line, seconds);
    push(&mut line, "s ");
    push(&mut line, what);
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
    push_u32(&mut line, fed.portions as u32);
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
    push_u32(&mut line, slot.portions as u32);
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

fn push_u32(line: &mut Line, mut value: u32) {
    if value == 0 {
        let _ = line.push('0');
        return;
    }

    let mut digits = [0u8; 10];
    let mut n = 0;
    while value > 0 {
        digits[n] = b'0' + (value % 10) as u8;
        value /= 10;
        n += 1;
    }
    while n > 0 {
        n -= 1;
        let _ = line.push(digits[n] as char);
    }
}

fn push_ip(line: &mut Line, ip: [u8; 4]) {
    for (i, octet) in ip.iter().enumerate() {
        if i > 0 {
            push(line, ".");
        }
        push_u32(line, *octet as u32);
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

    const UNIT: UnitInfo<'static> = UnitInfo {
        id: "99177c",
        board: "zero",
        version: "0.1.0",
        wifi_ssid: "fdlgrm",
        mqtt_host: "192.168.68.126",
        mqtt_port: 1883,
        mqtt_user: "cat-feeder",
    };

    const CAL: Calibration = Calibration {
        portion_scale_pct: 100,
        detent_ms: 1900,
    };

    fn view() -> View<'static> {
        View {
            status: Status::Healthy,
            setup: None,
            last_fed: None,
            next: None,
            mode: Mode::default(),
            paused: false,
            net: Net::default(),
            unit: Some(UNIT),
            calibration: CAL,
            now: None,
            no_meals: false,
        }
    }

    fn page(page: Page) -> View<'static> {
        View {
            mode: Mode::Locked { page },
            ..view()
        }
    }

    fn menu(item: Item) -> View<'static> {
        View {
            status: Status::Armed,
            mode: Mode::Unlocked { item },
            ..view()
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
        assert_eq!(screen.lines[5], "HOLD 2s FOR MENU");
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
        assert_eq!(screen.lines[1], "fed  08:00  x2");
        assert_eq!(screen.lines[5], "HOLD 2s FOR MENU");
        assert_fits(&screen);
    }

    /// The whole point of carrying `mode` beside `status`.
    ///
    /// `Status::of` reports `Jammed` over `Armed`, so a screen driven by the
    /// status alone could not tell a jammed unit with its menu open from one
    /// without — and they are the two the person standing at a jammed feeder is
    /// trying to distinguish.
    #[test]
    fn a_jammed_menu_still_shouts_and_offers_a_retry() {
        let screen = render(&View {
            status: Status::Jammed,
            ..menu(Item::Feed)
        });

        assert_eq!(screen.lines[0], "** JAMMED **");
        assert_eq!(screen.lines[1], "> Retry feed");
        assert_fits(&screen);
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

    /// The hint follows [`ARM_HOLD_MS`] rather than a typed-in `2`.
    ///
    /// Exercised through [`hold_hint_for`] across durations, because a test
    /// that only ever sees the one value the constant currently holds cannot
    /// tell a derivation from a literal.
    #[test]
    fn the_hold_hint_never_asks_for_less_than_it_takes() {
        // An instruction may overstate a hold and must never understate one:
        // holding longer than the printed time always works, holding for
        // exactly a floored figure need not. 2_500 ms is the case that made
        // this explicit — flooring prints `2s`, and 2 s would open nothing.
        for hold_ms in [
            1, 500, 999, 1_000, 1_001, 2_000, 2_500, 2_999, 3_000, 10_000,
        ] {
            let line = hold_hint_for(hold_ms, "FOR MENU");
            let seconds = printed_seconds(&line, "FOR MENU") as u64;

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
    fn every_screen_renders_that_hint_rather_than_its_own() {
        for view in [view(), page(Page::Network), page(Page::Device)] {
            let screen = render(&view);
            assert_eq!(screen.lines[5], hold_hint_for(ARM_HOLD_MS, "FOR MENU"));
            assert_eq!(
                printed_seconds(&screen.lines[5], "FOR MENU") as u64 * 1_000,
                ARM_HOLD_MS
            );
        }

        let screen = render(&menu(Item::Feed));
        assert_eq!(screen.lines[5], hold_hint_for(ARM_HOLD_MS, "TO LOCK"));
        // Spelled out as well, so a reader sees what the panel says today.
        assert_eq!(screen.lines[5], "HOLD 2s TO LOCK");
    }

    /// Reads the number back out of a rendered hint.
    ///
    /// Measuring what the panel shows rather than what the arithmetic intended
    /// is also the only width check that can fail here: `Line` truncates at
    /// [`COLS`], so an over-long hint loses its tail and this stops parsing.
    /// Asserting `len() <= COLS` could never fail.
    fn printed_seconds(line: &Line, what: &str) -> u32 {
        line.strip_prefix("HOLD ")
            .and_then(|rest| rest.strip_suffix(what))
            .and_then(|rest| rest.strip_suffix("s "))
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

    // --- the menu -------------------------------------------------------------

    #[test]
    fn the_menu_points_at_the_item_a_tap_will_run() {
        let screen = render(&menu(Item::Feed));
        assert_eq!(screen.lines[0], "MENU");
        assert_eq!(screen.lines[1], "> Feed one portion");
        assert_eq!(screen.lines[2], "  Pause schedule");
        assert_eq!(screen.lines[3], "  Settings");
        assert_eq!(screen.lines[4], "  Lock");
        assert_fits(&screen);

        let screen = render(&menu(Item::Lock));
        assert_eq!(screen.lines[1], "  Feed one portion");
        assert_eq!(screen.lines[4], "> Lock");
    }

    // --- settings -------------------------------------------------------------

    fn in_mode(mode: Mode) -> View<'static> {
        View {
            status: Status::Armed,
            mode,
            ..view()
        }
    }

    #[test]
    fn the_settings_list_shows_what_each_value_is_now() {
        let screen = render(&View {
            now: Some(at(20, 14)),
            ..in_mode(Mode::Settings {
                item: Setting::Detent,
            })
        });

        assert_eq!(screen.lines[0], "SETTINGS");
        assert_eq!(screen.lines[1], "  Clock   20:14");
        assert_eq!(screen.lines[2], "  Portion x100%");
        assert_eq!(screen.lines[3], "> Detent  1900ms");
        assert_eq!(screen.lines[4], "  Calibrate");
        assert_eq!(screen.lines[5], "HOLD 2s TO LOCK");
        assert_fits(&screen);
    }

    /// Six items, four rows: the window follows the cursor to `Back`, and
    /// comes back to `Clock` when the cursor does.
    #[test]
    fn the_settings_list_scrolls_with_the_cursor() {
        let back = render(&in_mode(Mode::Settings {
            item: Setting::Back,
        }));
        assert_eq!(back.lines[1], "  Detent  1900ms");
        assert_eq!(back.lines[4], "> Back");

        let clock = render(&in_mode(Mode::Settings {
            item: Setting::Clock,
        }));
        assert_eq!(clock.lines[1], "> Clock   not set");
        assert_eq!(clock.lines[4], "  Calibrate");
    }

    fn at(hour: u32, minute: u32) -> Wall {
        Wall {
            second_of_day: hour * 3600 + minute * 60,
            ..wall(0, 0)
        }
    }

    #[test]
    fn the_clock_screen_underlines_the_field_the_knob_turns() {
        let edit = |field| ClockEdit {
            year: 2026,
            month: 9,
            day: 5,
            hour: 7,
            minute: 3,
            field,
        };

        let screen = render(&in_mode(Mode::SettingClock(edit(ClockField::Year))));
        assert_eq!(screen.lines[0], "SET CLOCK");
        assert_eq!(screen.lines[2], "2026-09-05  07:03");
        assert_eq!(screen.lines[3], "^^^^");
        assert_eq!(screen.lines[4], "TAP: NEXT");
        assert_eq!(screen.lines[5], "HOLD 2s TO CANCEL");
        assert_fits(&screen);

        for (field, mark) in [
            (ClockField::Month, "     ^^"),
            (ClockField::Day, "        ^^"),
            (ClockField::Hour, "            ^^"),
            (ClockField::Minute, "               ^^"),
        ] {
            let screen = render(&in_mode(Mode::SettingClock(edit(field))));
            assert_eq!(screen.lines[3], mark, "{field:?}");
        }

        let last = render(&in_mode(Mode::SettingClock(edit(ClockField::Minute))));
        assert_eq!(last.lines[4], "TAP TO SET");
    }

    /// Blank has to be visible: otherwise it looks exactly like healthy.
    #[test]
    fn a_unit_with_no_meals_says_so_where_healthy_says_nothing() {
        let screen = render(&View {
            no_meals: true,
            ..view()
        });
        assert_eq!(screen.lines[0], "NO MEALS SET");
        assert_fits(&screen);
    }

    /// A fault is the first thing to fix, and no meal comes while it lasts.
    #[test]
    fn a_fault_outranks_no_meals() {
        let screen = render(&View {
            status: Status::NoBroker,
            no_meals: true,
            ..view()
        });
        assert_eq!(screen.lines[0], "NO BROKER");
    }

    // --- calibration ------------------------------------------------------

    #[test]
    fn calibration_says_what_it_will_dispense_before_it_starts() {
        let screen = render(&in_mode(Mode::ConfirmCalibrate { start: false }));
        assert_eq!(screen.lines[0], "CALIBRATE");
        assert_eq!(screen.lines[1], "turns 5 detents and");
        assert_eq!(screen.lines[2], "dispenses 5 portions");
        assert_eq!(screen.lines[3], "> Keep");
        assert_eq!(screen.lines[4], "  Start");
        assert_eq!(screen.lines[5], "HOLD 2s TO CANCEL");
        assert_fits(&screen);
    }

    #[test]
    fn a_running_calibration_counts_its_clicks() {
        let screen = render(&in_mode(Mode::Calibrating { clicks: 3 }));
        assert_eq!(screen.lines[0], "CALIBRATING");
        assert_eq!(screen.lines[1], "click 3 of 5");
        assert_eq!(screen.lines[5], "HOLD 2s TO LOCK");
    }

    #[test]
    fn a_result_shows_new_against_now() {
        let screen = render(&in_mode(Mode::Calibrated(Ok(Measurement {
            detent_ms: 2_070,
            fastest_ms: 2_038,
            slowest_ms: 2_061,
        }))));
        assert_eq!(screen.lines[0], "CALIBRATED");
        assert_eq!(screen.lines[1], "new  2070ms");
        assert_eq!(screen.lines[2], "gaps 2038-2061ms");
        assert_eq!(screen.lines[3], "now  1900ms");
        assert_eq!(screen.lines[4], "TAP TO SAVE");
        assert_fits(&screen);
    }

    /// Every failure says why in words, and none of them offers to save.
    #[test]
    fn every_failure_says_why_and_fits() {
        for (failure, line) in [
            (Failure::Jammed, "no click in 10s"),
            (
                Failure::Inconsistent {
                    fastest_ms: 2_000,
                    slowest_ms: 4_000,
                },
                "clicks uneven",
            ),
            (Failure::TooFast { slowest_ms: 50 }, "too fast: bouncing?"),
            (
                Failure::TooSlow { slowest_ms: 5_200 },
                "too slow: stalling?",
            ),
        ] {
            let screen = render(&in_mode(Mode::Calibrated(Err(failure))));
            assert_eq!(screen.lines[0], "CALIBRATION FAILED");
            assert_eq!(screen.lines[1], line);
            assert_eq!(screen.lines[4], "TAP: BACK");
            assert_fits(&screen);
        }
    }

    /// The widest numbers a result can carry still fit.
    #[test]
    fn the_widest_calibration_numbers_fit() {
        let screen = render(&in_mode(Mode::Calibrated(Ok(Measurement {
            detent_ms: 5_000,
            fastest_ms: 4_000,
            slowest_ms: 4_999,
        }))));
        assert_eq!(screen.lines[2], "gaps 4000-4999ms");
        assert_fits(&screen);
    }

    #[test]
    fn the_home_page_shows_the_time_only_when_it_is_trusted() {
        assert_eq!(render(&view()).lines[3], "");
        let screen = render(&View {
            now: Some(at(20, 14)),
            ..view()
        });
        assert_eq!(screen.lines[3], "now  20:14");
    }

    #[test]
    fn an_edit_shows_now_and_new_and_warns_of_the_restart() {
        let screen = render(&in_mode(Mode::Editing {
            field: Field::PortionScale,
            value: 135,
        }));

        assert_eq!(screen.lines[0], "PORTION SIZE");
        assert_eq!(screen.lines[1], "now  x100%");
        assert_eq!(screen.lines[2], "new  x135%");
        assert_eq!(screen.lines[4], "TAP TO SAVE");
        assert_eq!(screen.lines[5], "HOLD 2s TO CANCEL");
        assert_fits(&screen);

        let screen = render(&in_mode(Mode::Editing {
            field: Field::Detent,
            value: 2_050,
        }));
        assert_eq!(screen.lines[1], "now  1900ms");
        assert_eq!(screen.lines[2], "new  2050ms");
    }

    #[test]
    fn the_reset_confirmation_points_at_its_choice() {
        let keep = render(&in_mode(Mode::ConfirmReset { erase: false }));
        // Both exactly twenty-one, so a word more would be cut off silently.
        assert_eq!(keep.lines[1], "erases Wi-Fi, broker,");
        assert_eq!(keep.lines[2], "calibration and meals");
        assert_eq!(keep.lines[3], "> Keep");
        assert_eq!(keep.lines[4], "  Erase, restart");
        assert_eq!(keep.lines[5], "HOLD 2s TO CANCEL");
        assert_fits(&keep);

        let erase = render(&in_mode(Mode::ConfirmReset { erase: true }));
        assert_eq!(erase.lines[3], "  Keep");
        assert_eq!(erase.lines[4], "> Erase, restart");
    }

    /// The widest values each field can take.
    #[test]
    fn the_extreme_values_fit() {
        for field in [Field::PortionScale, Field::Detent] {
            let (min, max, _) = field.range();
            for value in [min, max] {
                let screen = render(&in_mode(Mode::Editing { field, value }));
                assert_fits(&screen);
                assert!(screen.lines[2].ends_with('%') || screen.lines[2].ends_with("ms"));
            }
        }
    }

    /// The label says which way a tap flips it, and that comes from `paused`
    /// rather than the status, which an open menu hides behind `Armed`.
    #[test]
    fn a_paused_unit_offers_to_resume() {
        let screen = render(&View {
            paused: true,
            ..menu(Item::Pause)
        });
        assert_eq!(screen.lines[2], "> Resume schedule");
    }

    #[test]
    fn a_menu_tap_that_feeds_shows_it_happening() {
        let screen = render(&View {
            status: Status::Feeding,
            ..menu(Item::Feed)
        });
        assert_eq!(screen.lines[0], "FEEDING");
        assert_eq!(screen.lines[1], "> Feed one portion");
    }

    // --- the info pages -------------------------------------------------------

    #[test]
    fn the_network_page_says_where_this_unit_is() {
        let screen = render(&View {
            net: Net {
                link: true,
                broker: false,
                ip: Some([192, 168, 68, 105]),
            },
            ..page(Page::Network)
        });

        assert_eq!(screen.lines[0], "WI-FI             2/4");
        assert_eq!(screen.lines[1], "ip 192.168.68.105");
        assert_eq!(screen.lines[2], "connected");
        assert_eq!(screen.lines[3], "fdlgrm");
        assert_eq!(screen.lines[4], "");
        assert_fits(&screen);
    }

    #[test]
    fn an_address_not_yet_handed_out_says_so() {
        let screen = render(&page(Page::Network));
        assert_eq!(screen.lines[1], "ip none yet");
        assert_eq!(screen.lines[2], "not connected");
    }

    /// An SSID is up to 32 bytes, and a clipped one names a network that does
    /// not exist — so it wraps onto a second line rather than losing its tail.
    #[test]
    fn a_long_ssid_wraps_rather_than_being_clipped() {
        let ssid = "a-rather-long-home-network-name!"; // 32
        let screen = render(&View {
            unit: Some(UnitInfo {
                wifi_ssid: ssid,
                ..UNIT
            }),
            ..page(Page::Network)
        });

        let shown = alloc::format!("{}{}", screen.lines[3], screen.lines[4]);
        assert_eq!(shown, ssid);
        assert_fits(&screen);
    }

    #[test]
    fn a_long_user_wraps_after_its_label() {
        let user = "cat-feeder-upstairs-kitchen"; // 27
        let screen = render(&View {
            unit: Some(UnitInfo {
                mqtt_user: user,
                ..UNIT
            }),
            ..page(Page::Broker)
        });

        let shown = alloc::format!("{}{}", screen.lines[3], screen.lines[4]);
        assert_eq!(shown, alloc::format!("user {user}"));
    }

    #[test]
    fn the_broker_page_names_the_broker_and_never_its_password() {
        let screen = render(&View {
            net: Net {
                broker: true,
                ..Net::default()
            },
            ..page(Page::Broker)
        });

        assert_eq!(screen.lines[0], "BROKER            3/4");
        assert_eq!(screen.lines[1], "192.168.68.126:1883");
        assert_eq!(screen.lines[2], "connected");
        assert_eq!(screen.lines[3], "user cat-feeder");
        assert_fits(&screen);
    }

    /// The widest address the firmware accepts, and the widest port.
    #[test]
    fn the_longest_broker_address_fits_exactly() {
        let screen = render(&View {
            unit: Some(UnitInfo {
                mqtt_host: "255.255.255.255",
                mqtt_port: 65535,
                ..UNIT
            }),
            ..page(Page::Broker)
        });
        assert_eq!(screen.lines[1], "255.255.255.255:65535");
    }

    #[test]
    fn the_device_page_says_what_this_unit_was_calibrated_for() {
        let screen = render(&View {
            calibration: Calibration {
                detent_ms: 2048,
                portion_scale_pct: 133,
            },
            ..page(Page::Device)
        });

        assert_eq!(screen.lines[0], "DEVICE            4/4");
        assert_eq!(screen.lines[1], "id 99177c  zero");
        assert_eq!(screen.lines[2], "fw 0.1.0");
        assert_eq!(screen.lines[3], "detent 2048ms");
        assert_eq!(screen.lines[4], "portion x133%");
        assert_fits(&screen);
    }

    // --- setup ----------------------------------------------------------------

    #[test]
    fn setup_mode_replaces_the_whole_screen() {
        let screen = render(&View {
            status: Status::Setup,
            setup: Some(SetupInfo {
                ssid: "cat-feeder-99177c",
                password: "H75T-C7VT-6FAV",
            }),
            unit: None,
            ..view()
        });

        assert_eq!(screen.lines[1], "cat-feeder-99177c");
        assert_eq!(screen.lines[2], "H75T-C7VT-6FAV");
        // `AP_URL`, not the string it happens to hold: `setup.rs` binds a
        // socket built from the same octets, and a second literal here would be
        // a second place for that address to be wrong.
        assert_eq!(screen.lines[5], crate::provisioning::AP_URL);
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

        assert_eq!(screen.lines[1], ssid.as_str(), "the SSID was clipped");
        assert_eq!(
            screen.lines[2],
            password.as_str(),
            "the password was clipped"
        );
        assert_eq!(screen.lines[5], AP_URL);
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

        assert_eq!(screen.lines[1], "cat-feeder-with-a-ver");
        assert_fits(&screen);
    }

    // --- the home page's lines ------------------------------------------------

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

    const EVERY_STATUS: [Status; 9] = [
        Status::Jammed,
        Status::Feeding,
        Status::Armed,
        Status::Setup,
        Status::NoLink,
        Status::NoBroker,
        Status::NoTime,
        Status::Paused,
        Status::Healthy,
    ];

    /// Every rung of the ladder, on every page and every menu position, must
    /// fit — including the longest wording. This is the test that catches a
    /// banner or a label edited to something a shade too long.
    #[test]
    fn every_screen_fits_the_panel() {
        for status in EVERY_STATUS {
            for mode in Page::ALL
                .map(|page| Mode::Locked { page })
                .into_iter()
                .chain(Item::ALL.map(|item| Mode::Unlocked { item }))
                .chain(Setting::ALL.map(|item| Mode::Settings { item }))
                .chain([
                    Mode::ConfirmReset { erase: false },
                    Mode::ConfirmReset { erase: true },
                    Mode::Editing {
                        field: Field::Detent,
                        value: 5_000,
                    },
                ])
            {
                for paused in [false, true] {
                    let screen = render(&View {
                        status,
                        mode,
                        paused,
                        ..view()
                    });
                    assert_fits(&screen);
                }
            }
        }
    }

    /// Only `Healthy` is allowed to say nothing, or the rule the module is
    /// built on — an empty top line means all is well — stops being true.
    #[test]
    fn only_a_healthy_unit_has_an_empty_banner() {
        for status in EVERY_STATUS {
            if matches!(status, Status::Healthy | Status::Setup) {
                continue;
            }
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
}
