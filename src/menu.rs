//! What the knob and its click mean: pages while locked, menus while unlocked.
//!
//! Pure logic, layered on [`Button`]. The button still owns the one rule that
//! keeps cats fed the right amount — no short press does anything until a
//! deliberate hold has come first — and this module decides what a press or a
//! turn means *given* that.
//!
//! | | Turn | Tap | Hold |
//! |---|---|---|---|
//! | locked | steps the info pages | back to the home page | unlock → menu, on `Feed` |
//! | menu or settings | moves the cursor | runs the item | lock |
//! | editing a number | changes it | saves it, applied at once | lock, discarding it |
//! | confirming a reset | `Keep` or `Erase` | runs the choice | lock, keeping everything |
//! | nothing for ten seconds | | | locks again |
//!
//! Rules, each a test below:
//!
//! - **Turning never dispenses and never saves.** Food needs a tap on `Feed`,
//!   a saved setting needs a tap on the edit screen, and both are behind a
//!   hold. A cat batting the knob steps pages and lights the screen.
//! - **`Feed` stays under the cursor after feeding**, so three portions is three
//!   taps. There is no portion count held between taps, which is the decision
//!   *Manual feeds accumulate* in `CLAUDE.md` records.
//! - **A hold only ever unlocks or locks.** Leaving an edit is a hold or the
//!   window lapsing, and both throw the edit away; nothing is saved except by a
//!   tap on the value itself.
//! - **Erasing starts on `Keep`.** A tap too many on `Factory reset` keeps
//!   everything; erasing needs a deliberate turn first.
//! - **Waking lands on the home page.** A turn that lights a sleeping panel
//!   only lights it; otherwise the first thing seen would be whatever page was
//!   left days ago, which reads as a screen stuck on the wrong thing.

use crate::button::{Button, Event};
use crate::provisioning::MIN_DETENT_MS;

/// The pages a locked unit steps through, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Status, last feed, next feed.
    Home,
    /// The SSID and this unit's address.
    Network,
    /// The broker's address and user.
    Broker,
    /// Device id, firmware, and what this unit was calibrated for.
    Device,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Home, Page::Network, Page::Broker, Page::Device];

    /// Position in [`Page::ALL`], for the `2/4` in a page's title.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// What the unlocked menu offers, in order. The first is where unlocking lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// One portion per tap.
    Feed,
    /// Pause the schedule, or resume it. Manual feeding is unaffected either way.
    Pause,
    /// This unit's calibration, and the factory reset.
    Settings,
    /// Lock now rather than waiting the window out.
    Lock,
}

impl Item {
    pub const ALL: [Item; 4] = [Item::Feed, Item::Pause, Item::Settings, Item::Lock];
}

/// What the settings menu offers, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Edit [`Field::PortionScale`].
    PortionScale,
    /// Edit [`Field::Detent`].
    Detent,
    /// Erase the record, after a confirmation.
    Reset,
    /// Back to the main menu.
    Back,
}

impl Setting {
    pub const ALL: [Setting; 4] = [
        Setting::PortionScale,
        Setting::Detent,
        Setting::Reset,
        Setting::Back,
    ];
}

/// A number the knob can edit. Both live in the flash record beside the
/// credentials, and both take effect without a restart: the feeder task picks
/// the new figures up at its next idle moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `portions::clicks_for`'s scale, in percent.
    PortionScale,
    /// The detent interval every mechanical timing is derived from, in ms.
    Detent,
}

impl Field {
    /// The smallest, largest, and one detent's worth of change.
    ///
    /// Detent's floor is the record's own, so the knob cannot store a value
    /// the record would then refuse to read. The ceilings are well past any
    /// mechanism measured, and within what `feeder.rs`'s tests cover.
    pub const fn range(self) -> (u16, u16, u16) {
        match self {
            Field::PortionScale => (25, 300, 5),
            Field::Detent => (MIN_DETENT_MS, 5_000, 10),
        }
    }

    fn setting(self) -> Setting {
        match self {
            Field::PortionScale => Setting::PortionScale,
            Field::Detent => Setting::Detent,
        }
    }
}

/// This unit's current calibration, as booted. The "now" beside an edit.
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

/// What the screen is showing, for `display.rs` to lay out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Locked { page: Page },
    Unlocked { item: Item },
    Settings { item: Setting },
    Editing { field: Field, value: u16 },
    ConfirmReset { erase: bool },
}

impl Mode {
    pub fn is_locked(self) -> bool {
        matches!(self, Mode::Locked { .. })
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Locked { page: Page::Home }
    }
}

/// What an input turned out to mean.
///
/// Actions for the caller to carry out, and plain changes for it to log. Every
/// variant means the screen should be redrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Held long enough: the menu is open.
    Unlocked,
    /// Locked by a hold, or by the menu's `Lock`.
    Locked,
    /// The window lapsed with nothing touched.
    Expired,
    /// Dispense one portion.
    Feed,
    /// Flip the schedule's pause.
    TogglePause,
    /// Store this value in the record and apply it. The caller reports back
    /// with [`Menu::saved`] once it has landed.
    Save { field: Field, value: u16 },
    /// Erase the record, then restart into setup mode.
    FactoryReset,
    /// The page, the cursor or the value moved.
    Moved(Mode),
    /// A locked tap: back to the home page.
    Home,
    /// A turn that only woke the screen.
    Woke,
}

/// The knob, the click, and what they currently mean.
#[derive(Debug)]
pub struct Menu {
    button: Button,
    mode: Mode,
    calibration: Calibration,
}

impl Menu {
    pub const fn new(calibration: Calibration) -> Self {
        Self {
            button: Button::new(),
            mode: Mode::Locked { page: Page::Home },
            calibration,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn calibration(&self) -> Calibration {
        self.calibration
    }

    /// The caller stored a [`Outcome::Save`]. The new figure becomes the "now"
    /// beside the next edit, and the cursor goes back to the list.
    ///
    /// Separate from `Save` itself because the write can fail, and the menu
    /// must not show a value the flash does not hold. On a failure the caller
    /// simply does not call this, and the edit stays on screen.
    pub fn saved(&mut self, field: Field, value: u16) -> Outcome {
        match field {
            Field::PortionScale => self.calibration.portion_scale_pct = value,
            Field::Detent => self.calibration.detent_ms = value,
        }
        self.mode = Mode::Settings {
            item: field.setting(),
        };
        Outcome::Moved(self.mode)
    }

    /// Whether a menu is open. Read by the LED, which blinks cyan for it.
    pub fn is_unlocked(&self) -> bool {
        self.button.is_armed()
    }

    /// The click's settled level changed.
    pub fn on_change(&mut self, now_ms: u64, pressed: bool) -> Option<Outcome> {
        let event = self.button.on_change(now_ms, pressed)?;
        Some(self.on_event(event))
    }

    /// Time passed. Holds unlock and lock while still held, and the window lapses.
    pub fn poll(&mut self, now_ms: u64) -> Option<Outcome> {
        let event = self.button.poll(now_ms)?;
        Some(self.on_event(event))
    }

    /// The knob moved `steps` detents. `screen_awake` is whether the panel was
    /// lit *before* this turn.
    pub fn on_turn(&mut self, now_ms: u64, steps: i8, screen_awake: bool) -> Option<Outcome> {
        if steps == 0 {
            return None;
        }

        let before = self.mode;
        if let Mode::Locked { page } = self.mode {
            if !screen_awake {
                self.mode = Mode::default();
                return Some(Outcome::Woke);
            }
            self.mode = Mode::Locked {
                page: along(&Page::ALL, page, steps),
            };
        } else {
            // Turning is attention, so it keeps the menu open.
            self.button.refresh(now_ms);
            self.mode = match self.mode {
                Mode::Unlocked { item } => Mode::Unlocked {
                    item: along(&Item::ALL, item, steps),
                },
                Mode::Settings { item } => Mode::Settings {
                    item: along(&Setting::ALL, item, steps),
                },
                Mode::Editing { field, value } => {
                    let (min, max, step) = field.range();
                    let value = value as i32 + steps as i32 * step as i32;
                    Mode::Editing {
                        field,
                        value: value.clamp(min as i32, max as i32) as u16,
                    }
                }
                // Clockwise is down the screen, and `Erase` is below `Keep`.
                Mode::ConfirmReset { .. } => Mode::ConfirmReset { erase: steps > 0 },
                Mode::Locked { .. } => unreachable!("handled above"),
            };
        }

        // Turning against an end stop still refreshed the window above, but it
        // moved nothing, so there is nothing to redraw or log.
        (self.mode != before).then_some(Outcome::Moved(self.mode))
    }

    fn on_event(&mut self, event: Event) -> Outcome {
        match event {
            Event::Armed => {
                self.mode = Mode::Unlocked { item: Item::Feed };
                Outcome::Unlocked
            }
            Event::Locked => {
                self.mode = Mode::default();
                Outcome::Locked
            }
            Event::Expired => {
                self.mode = Mode::default();
                Outcome::Expired
            }
            Event::Ignored => {
                self.mode = Mode::default();
                Outcome::Home
            }
            Event::Select => self.select(),
        }
    }

    /// A tap while unlocked: run whatever is under the cursor.
    fn select(&mut self) -> Outcome {
        let next = match self.mode {
            Mode::Unlocked { item: Item::Feed } => return Outcome::Feed,
            Mode::Unlocked { item: Item::Pause } => return Outcome::TogglePause,
            Mode::Unlocked { item: Item::Lock } => {
                self.button.lock();
                self.mode = Mode::default();
                return Outcome::Locked;
            }
            Mode::Unlocked {
                item: Item::Settings,
            } => Mode::Settings {
                item: Setting::PortionScale,
            },

            Mode::Settings {
                item: Setting::PortionScale,
            } => self.edit(Field::PortionScale),
            Mode::Settings {
                item: Setting::Detent,
            } => self.edit(Field::Detent),
            Mode::Settings {
                item: Setting::Reset,
            } => Mode::ConfirmReset { erase: false },
            Mode::Settings {
                item: Setting::Back,
            } => Mode::Unlocked {
                item: Item::Settings,
            },

            Mode::Editing { field, value } => {
                // A value left where it was is not worth a flash write: back
                // to the list instead.
                if value != self.calibration.get(field) {
                    return Outcome::Save { field, value };
                }
                Mode::Settings {
                    item: field.setting(),
                }
            }

            Mode::ConfirmReset { erase: true } => return Outcome::FactoryReset,
            Mode::ConfirmReset { erase: false } => Mode::Settings {
                item: Setting::Reset,
            },

            // The button says armed and the mode says locked: they are only
            // ever changed together, so this is unreachable. Doing nothing is
            // the answer that cannot feed by accident.
            Mode::Locked { .. } => return Outcome::Home,
        };

        self.mode = next;
        Outcome::Moved(next)
    }

    fn edit(&self, field: Field) -> Mode {
        Mode::Editing {
            field,
            value: self.calibration.get(field),
        }
    }
}

/// Moves along a list, stopping at the ends rather than wrapping.
///
/// Stopping is what makes a menu learnable by feel: turn left until it stops
/// and the cursor is on the first item, whatever it was on before.
fn along<T: Copy + PartialEq>(list: &[T], from: T, steps: i8) -> T {
    let at = list.iter().position(|x| *x == from).unwrap_or(0) as i32;
    list[(at + steps as i32).clamp(0, list.len() as i32 - 1) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::button::{ARM_HOLD_MS, ARMED_WINDOW_MS};

    const CAL: Calibration = Calibration {
        portion_scale_pct: 100,
        detent_ms: 1_900,
    };

    /// A menu, a clock, and a record of what came out.
    struct Bench {
        menu: Menu,
        now: u64,
        out: alloc::vec::Vec<Outcome>,
    }

    impl Bench {
        fn new() -> Self {
            Self {
                menu: Menu::new(CAL),
                now: 1_000,
                out: alloc::vec![],
            }
        }

        fn wait(&mut self, ms: u64) {
            let until = self.now + ms;
            while self.now < until {
                self.now = (self.now + 20).min(until);
                if let Some(o) = self.menu.poll(self.now) {
                    self.out.push(o);
                }
            }
        }

        fn press(&mut self, held: u64) {
            if let Some(o) = self.menu.on_change(self.now, true) {
                self.out.push(o);
            }
            self.wait(held);
            if let Some(o) = self.menu.on_change(self.now, false) {
                self.out.push(o);
            }
            self.wait(200);
        }

        fn tap(&mut self) {
            self.press(80);
        }

        fn hold(&mut self) {
            self.press(ARM_HOLD_MS + 100);
        }

        fn turn(&mut self, steps: i8) {
            if let Some(o) = self.menu.on_turn(self.now, steps, true) {
                self.out.push(o);
            }
        }

        fn feeds(&self) -> usize {
            self.out.iter().filter(|o| **o == Outcome::Feed).count()
        }
    }

    #[test]
    fn unlocking_lands_on_feed_and_a_tap_feeds() {
        let mut b = Bench::new();
        b.hold();
        assert_eq!(b.menu.mode(), Mode::Unlocked { item: Item::Feed });

        b.tap();
        assert_eq!(b.feeds(), 1);
    }

    #[test]
    fn three_taps_on_feed_are_three_portions() {
        let mut b = Bench::new();
        b.hold();
        b.tap();
        b.tap();
        b.tap();

        assert_eq!(b.feeds(), 3);
        assert_eq!(b.menu.mode(), Mode::Unlocked { item: Item::Feed });
    }

    /// The property the whole design rests on. Turning, tapping and turning
    /// again in every combination a paw could manage, without a hold, and not
    /// one portion.
    #[test]
    fn nothing_but_a_hold_then_a_tap_can_feed() {
        let mut b = Bench::new();
        for i in 0..50 {
            b.turn(if i % 3 == 0 { -2 } else { 1 });
            b.tap();
            b.wait(700);
        }

        assert_eq!(b.feeds(), 0);
        assert!(!b.menu.is_unlocked());
    }

    #[test]
    fn turning_while_locked_steps_the_pages_and_stops_at_the_ends() {
        let mut b = Bench::new();
        b.turn(1);
        assert_eq!(
            b.menu.mode(),
            Mode::Locked {
                page: Page::Network
            }
        );
        b.turn(1);
        b.turn(1);
        b.turn(1);
        b.turn(5);
        assert_eq!(b.menu.mode(), Mode::Locked { page: Page::Device });
        b.turn(-10);
        assert_eq!(b.menu.mode(), Mode::Locked { page: Page::Home });
    }

    #[test]
    fn a_locked_tap_goes_home() {
        let mut b = Bench::new();
        b.turn(2);
        b.tap();
        assert_eq!(b.menu.mode(), Mode::Locked { page: Page::Home });
        assert_eq!(b.out.last(), Some(&Outcome::Home));
    }

    #[test]
    fn a_turn_on_a_dark_screen_only_wakes_it() {
        let mut m = Menu::new(CAL);
        assert_eq!(
            m.on_turn(0, 1, true),
            Some(Outcome::Moved(Mode::Locked {
                page: Page::Network
            }))
        );

        // Days later, the panel asleep: the turn lights it and shows home,
        // rather than advancing from the page left behind.
        assert_eq!(m.on_turn(1_000_000, 1, false), Some(Outcome::Woke));
        assert_eq!(m.mode(), Mode::Locked { page: Page::Home });
    }

    #[test]
    fn turning_while_unlocked_moves_the_cursor_and_does_not_feed() {
        let mut b = Bench::new();
        b.hold();
        b.turn(1);
        assert_eq!(b.menu.mode(), Mode::Unlocked { item: Item::Pause });
        b.turn(1);
        b.turn(1);
        assert_eq!(b.menu.mode(), Mode::Unlocked { item: Item::Lock });
        b.turn(-5);
        assert_eq!(b.menu.mode(), Mode::Unlocked { item: Item::Feed });

        assert_eq!(b.feeds(), 0);
    }

    #[test]
    fn a_tap_on_pause_toggles_it_and_does_not_feed() {
        let mut b = Bench::new();
        b.hold();
        b.turn(1);
        b.tap();

        assert_eq!(b.out.last(), Some(&Outcome::TogglePause));
        assert_eq!(b.feeds(), 0);
        assert!(b.menu.is_unlocked(), "pausing does not close the menu");
    }

    #[test]
    fn the_lock_item_locks_and_nothing_expires_later() {
        let mut b = Bench::new();
        b.hold();
        b.turn(3);
        b.tap();

        assert_eq!(b.out.last(), Some(&Outcome::Locked));
        assert!(!b.menu.is_unlocked());
        assert_eq!(b.menu.mode(), Mode::Locked { page: Page::Home });

        b.out.clear();
        b.wait(ARMED_WINDOW_MS * 3);
        assert!(
            b.out.is_empty(),
            "something fired after locking: {:?}",
            b.out
        );
    }

    #[test]
    fn a_hold_in_the_menu_locks_wherever_the_cursor_is() {
        let mut b = Bench::new();
        b.hold();
        b.turn(1);
        b.hold();

        assert_eq!(b.out.last(), Some(&Outcome::Locked));
        assert_eq!(b.menu.mode(), Mode::Locked { page: Page::Home });
    }

    #[test]
    fn the_menu_locks_itself_when_left_alone() {
        let mut b = Bench::new();
        b.hold();
        b.wait(ARMED_WINDOW_MS + 100);

        assert_eq!(b.out.last(), Some(&Outcome::Expired));
        assert_eq!(b.menu.mode(), Mode::Locked { page: Page::Home });
    }

    /// Turning is attention: somebody reading the menu is not done with it.
    #[test]
    fn turning_keeps_the_menu_open() {
        let mut b = Bench::new();
        b.hold();
        for _ in 0..5 {
            b.wait(ARMED_WINDOW_MS / 2);
            b.turn(1);
            b.turn(-1);
        }

        assert!(b.menu.is_unlocked());
        assert!(!b.out.contains(&Outcome::Expired));
    }

    /// Reopening always starts on `Feed`, never on the item left last time —
    /// otherwise a hold then a tap would do something different every time.
    #[test]
    fn unlocking_again_starts_on_feed() {
        let mut b = Bench::new();
        b.hold();
        b.turn(1);
        b.hold();
        b.hold();

        assert_eq!(b.menu.mode(), Mode::Unlocked { item: Item::Feed });
    }

    /// Spinning against an end stop is a knob being fiddled with, not a
    /// change: nothing to redraw and nothing to log, but the menu stays open.
    #[test]
    fn turning_against_an_end_stop_is_silent_but_keeps_the_menu_open() {
        let mut b = Bench::new();
        b.hold();
        b.out.clear();
        for _ in 0..3 {
            b.wait(ARMED_WINDOW_MS / 2);
            b.turn(-1);
        }

        assert!(b.out.is_empty(), "{:?}", b.out);
        assert!(b.menu.is_unlocked());
    }

    // --- settings -------------------------------------------------------------

    fn saves(b: &Bench) -> alloc::vec::Vec<(Field, u16)> {
        b.out
            .iter()
            .filter_map(|o| match o {
                Outcome::Save { field, value } => Some((*field, *value)),
                _ => None,
            })
            .collect()
    }

    /// Hold, then turn to `Settings` and tap into it.
    fn into_settings(b: &mut Bench) {
        b.hold();
        b.turn(2);
        b.tap();
        assert_eq!(
            b.menu.mode(),
            Mode::Settings {
                item: Setting::PortionScale
            }
        );
    }

    #[test]
    fn editing_the_portion_scale_saves_on_a_tap() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.tap();
        assert_eq!(
            b.menu.mode(),
            Mode::Editing {
                field: Field::PortionScale,
                value: 100
            }
        );

        b.turn(1);
        b.turn(6);
        b.tap();

        assert_eq!(saves(&b), [(Field::PortionScale, 135)]);
        assert_eq!(b.feeds(), 0);
    }

    /// Once saved, the new value is what the next edit starts from and what
    /// an unchanged tap compares against.
    #[test]
    fn a_saved_value_becomes_the_current_one() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.tap();
        b.turn(2);
        b.tap();
        assert_eq!(saves(&b), [(Field::PortionScale, 110)]);

        b.menu.saved(Field::PortionScale, 110);
        assert_eq!(b.menu.calibration().portion_scale_pct, 110);
        assert_eq!(
            b.menu.mode(),
            Mode::Settings {
                item: Setting::PortionScale
            }
        );

        b.tap();
        assert_eq!(
            b.menu.mode(),
            Mode::Editing {
                field: Field::PortionScale,
                value: 110
            }
        );
        b.tap();
        assert_eq!(saves(&b).len(), 1, "an unchanged value was saved again");
    }

    #[test]
    fn editing_the_detent_moves_in_its_own_steps() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.turn(1);
        b.tap();
        b.turn(15);
        b.tap();

        assert_eq!(saves(&b), [(Field::Detent, 2_050)]);
    }

    #[test]
    fn a_value_stops_at_its_limits() {
        for field in [Field::PortionScale, Field::Detent] {
            let (min, max, _) = field.range();
            let mut m = Menu::new(CAL);
            m.mode = m.edit(field);
            m.button = {
                let mut b = Button::new();
                b.on_change(0, true);
                b.poll(ARM_HOLD_MS);
                b.on_change(ARM_HOLD_MS, false);
                b
            };

            for _ in 0..100 {
                m.on_turn(0, 127, true);
            }
            assert_eq!(m.mode(), Mode::Editing { field, value: max });
            for _ in 0..100 {
                m.on_turn(0, -127, true);
            }
            assert_eq!(m.mode(), Mode::Editing { field, value: min });
        }
    }

    /// The knob's floor is the record's own, so it can never store a detent
    /// the record would then refuse to read.
    #[test]
    fn the_detent_floor_is_the_records() {
        assert_eq!(Field::Detent.range().0, MIN_DETENT_MS);
    }

    /// A save restarts the unit, so one that changes nothing is not worth it.
    #[test]
    fn saving_the_unchanged_value_just_goes_back() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.tap();
        b.turn(3);
        b.turn(-3);
        b.tap();

        assert!(saves(&b).is_empty());
        assert_eq!(
            b.menu.mode(),
            Mode::Settings {
                item: Setting::PortionScale
            }
        );
    }

    /// Leaving an edit by a hold or by waiting throws it away.
    #[test]
    fn an_abandoned_edit_is_not_saved() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.tap();
        b.turn(4);
        b.hold();
        assert_eq!(b.out.last(), Some(&Outcome::Locked));

        into_settings(&mut b);
        b.tap();
        b.turn(4);
        b.wait(ARMED_WINDOW_MS + 100);
        assert_eq!(b.out.last(), Some(&Outcome::Expired));

        assert!(saves(&b).is_empty());
    }

    #[test]
    fn back_returns_to_the_main_menu_on_settings() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.turn(3);
        b.tap();

        assert_eq!(
            b.menu.mode(),
            Mode::Unlocked {
                item: Item::Settings
            }
        );
    }

    /// A tap too many must not erase anything.
    #[test]
    fn a_reset_starts_on_keep_and_keep_erases_nothing() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.turn(2);
        b.tap();
        assert_eq!(b.menu.mode(), Mode::ConfirmReset { erase: false });

        b.tap();
        assert!(!b.out.contains(&Outcome::FactoryReset));
        assert_eq!(
            b.menu.mode(),
            Mode::Settings {
                item: Setting::Reset
            }
        );
    }

    #[test]
    fn erasing_needs_a_turn_then_a_tap() {
        let mut b = Bench::new();
        into_settings(&mut b);
        b.turn(2);
        b.tap();
        b.turn(1);
        assert_eq!(b.menu.mode(), Mode::ConfirmReset { erase: true });
        b.tap();

        assert_eq!(b.out.last(), Some(&Outcome::FactoryReset));
    }

    /// The property the whole design rests on, extended to the settings: no
    /// sequence of turns and taps without a hold saves or erases anything.
    #[test]
    fn nothing_but_a_hold_can_reach_a_setting() {
        let mut b = Bench::new();
        for i in 0..60 {
            b.turn(if i % 4 == 0 { -3 } else { 2 });
            b.tap();
            b.wait(500);
        }

        assert!(saves(&b).is_empty());
        assert!(!b.out.contains(&Outcome::FactoryReset));
    }

    #[test]
    fn the_page_index_matches_the_order() {
        for (i, page) in Page::ALL.iter().enumerate() {
            assert_eq!(page.index(), i);
        }
    }
}
