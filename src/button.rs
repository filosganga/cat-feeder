//! The outside button: what a press means.
//!
//! Pure logic. Levels and timestamps go in, intentions come out, so every rule
//! below is host-tested rather than discovered by standing at a feeder pressing
//! things. `switch.rs` supplies the debounced levels; this decides what they
//! meant.
//!
//! ## The adversary is cats, not clumsiness
//!
//! A button on the outside of a cat feeder that dispenses food when pressed is
//! a button cats will learn to press. That is not a hypothetical — food is the
//! strongest reinforcer there is, and a cat has all day to experiment. It is
//! the reason this module exists at all rather than a short press simply
//! feeding.
//!
//! So a press does nothing until the button is **armed**, which takes a
//! deliberate two-second hold. A paw resting on it arms nothing useful, because
//! arming alone dispenses no food; a cat would have to hold for two seconds and
//! *then* tap within the window. The arm lapses on its own ten seconds later.
//!
//! **Firmware is the second line of defence, not the first.** Recessing the
//! button so it needs a fingertip defeats a paw outright and cannot be
//! defeated by a lucky sequence. Do both.
//!
//! ## Why reset is not a gesture here
//!
//! Erasing the configuration is the one irreversible thing this button could
//! do, and putting it on the same button as feeding means separating them by
//! hold duration — hold two seconds to arm, ten to wipe. The failure mode is
//! obvious: hold a beat too long on a working feeder and its credentials are
//! gone, with three units already screwed into place.
//!
//! So reset is not a runtime gesture at all. It is **held while powering on**,
//! the convention every router uses: it cannot happen by accident, because it
//! needs a deliberate power cycle to even become possible. See
//! [`held_at_boot`].
//!
//! That leaves two runtime gestures, distinguished by *kind* rather than by
//! duration: a long press arms, a short press feeds.

/// How long the button must be held to arm it.
///
/// Long enough to be deliberate, short enough not to feel broken. The LED
/// starts blinking the moment this is reached rather than on release, so the
/// hold has visible feedback and you know when to let go.
pub const ARM_HOLD_MS: u64 = 2_000;

/// How long the button stays armed with nothing pressed.
///
/// Every feed refreshes it, so three portions is three taps rather than three
/// arm-and-tap cycles.
pub const ARMED_WINDOW_MS: u64 = 10_000;

/// How long the button must be held **at power-on** to erase the record.
///
/// Longer than [`ARM_HOLD_MS`], but the duration is not what protects it — the
/// power cycle is. See the module docs.
pub const BOOT_RESET_HOLD_MS: u64 = 3_000;

/// What a press turned out to mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Held long enough. The LED should start saying so.
    Armed,
    /// The arm window lapsed with nothing pressed.
    Expired,
    /// A short press while armed: one portion.
    Feed,
    /// A short press while locked. Deliberately does nothing, but is worth a
    /// log line — it is the difference between "the button is broken" and "the
    /// button is working and you did not arm it".
    Ignored,
    /// Held long enough **while armed**: locked again, deliberately, without
    /// waiting the window out.
    ///
    /// Distinct from [`Event::Expired`] because the cause differs and so does
    /// what it tells you: one is somebody deciding they are done, the other is
    /// ten seconds passing. Same resulting state, different line on the
    /// console.
    Locked,
}

/// Turns debounced levels into [`Event`]s.
///
/// Two entry points because two different things generate events: a level
/// changing, and time passing while nothing changes. Arming has to fire *while*
/// the button is still held, and the window has to lapse with nothing pressed
/// at all, so neither can be driven by edges alone.
#[derive(Debug, Default)]
pub struct Button {
    /// When the current press started, if one is in progress.
    pressed_since: Option<u64>,
    /// Whether the current press has already toggled the mode, either way.
    ///
    /// Stops one long hold from firing repeatedly, stops its release counting
    /// as a short press, and — since a hold now locks as well as arms — stops a
    /// four-second hold arming at two seconds and locking at four, which would
    /// read as a button that does nothing.
    toggled_this_press: bool,
    /// When the arm lapses.
    armed_until: Option<u64>,
}

impl Button {
    pub const fn new() -> Self {
        Self {
            pressed_since: None,
            toggled_this_press: false,
            armed_until: None,
        }
    }

    /// The button's level changed. `pressed` is the settled new level.
    pub fn on_change(&mut self, now_ms: u64, pressed: bool) -> Option<Event> {
        if pressed {
            self.pressed_since = Some(now_ms);
            self.toggled_this_press = false;
            return None;
        }

        let since = self.pressed_since.take()?;

        // The hold that armed this button is not also a feed. Without this, one
        // long press would arm and then immediately spend the arm.
        if self.toggled_this_press {
            return None;
        }

        // Anything shorter than the arm hold is a tap. There is no third
        // duration to tell apart, which is the whole point of moving reset to
        // power-on.
        debug_assert!(now_ms.saturating_sub(since) < ARM_HOLD_MS);

        if self.armed_until.is_some() {
            // Feeding refreshes the window rather than spending it.
            self.armed_until = Some(now_ms + ARMED_WINDOW_MS);
            Some(Event::Feed)
        } else {
            Some(Event::Ignored)
        }
    }

    /// Time passed. Call this regularly; it is what arms and what expires.
    pub fn poll(&mut self, now_ms: u64) -> Option<Event> {
        // Arming wins over expiry: a hold that starts just as the window lapses
        // should arm, not report the lapse and drop the press on the floor.
        if let Some(since) = self.pressed_since
            && !self.toggled_this_press
            && now_ms.saturating_sub(since) >= ARM_HOLD_MS
        {
            self.toggled_this_press = true;

            // **A hold toggles the mode; a tap does whatever the mode means.**
            // That symmetry is the whole vocabulary, and it is what gives the
            // armed state a way out other than waiting: previously the only
            // exit was the ten-second window lapsing, so a change of mind meant
            // standing next to a live feeder doing nothing.
            return if self.armed_until.take().is_some() {
                Some(Event::Locked)
            } else {
                self.armed_until = Some(now_ms + ARMED_WINDOW_MS);
                Some(Event::Armed)
            };
        }

        if let Some(until) = self.armed_until
            && now_ms >= until
        {
            self.armed_until = None;
            return Some(Event::Expired);
        }

        None
    }

    /// Whether a tap would feed right now. Read by the LED.
    pub fn is_armed(&self) -> bool {
        self.armed_until.is_some()
    }
}

/// Whether the button was held long enough at power-on to mean "erase".
///
/// A plain fold over samples rather than anything stateful, because it runs
/// once and the answer is needed before the rest of the firmware starts. The
/// caller supplies levels at a known interval; any release at all is an
/// immediate no, so a brush against the button during a power cycle cannot
/// accumulate across two separate touches.
pub fn held_at_boot(samples: impl IntoIterator<Item = bool>, interval_ms: u64) -> bool {
    let mut held_ms = 0;

    for pressed in samples {
        if !pressed {
            return false;
        }
        held_ms += interval_ms;
        if held_ms >= BOOT_RESET_HOLD_MS {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a whole press: down at `at`, up `held` later, polling throughout.
    ///
    /// Mirrors what the task does, so the tests exercise the same interleaving
    /// of `poll` and `on_change` rather than an idealised one.
    fn press(button: &mut Button, at: u64, held: u64, events: &mut alloc::vec::Vec<(u64, Event)>) {
        if let Some(e) = button.on_change(at, true) {
            events.push((at, e));
        }
        let mut t = at;
        while t < at + held {
            t = (t + 25).min(at + held);
            if let Some(e) = button.poll(t) {
                events.push((t, e));
            }
        }
        if let Some(e) = button.on_change(at + held, false) {
            events.push((at + held, e));
        }
    }

    /// Runs time forward with nothing pressed.
    fn idle(button: &mut Button, from: u64, to: u64, events: &mut alloc::vec::Vec<(u64, Event)>) {
        let mut t = from;
        while t < to {
            t = (t + 25).min(to);
            if let Some(e) = button.poll(t) {
                events.push((t, e));
            }
        }
    }

    fn kinds(events: &[(u64, Event)]) -> alloc::vec::Vec<Event> {
        events.iter().map(|(_, e)| *e).collect()
    }

    #[test]
    fn a_tap_on_a_locked_button_does_nothing() {
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 1_000, 80, &mut events);

        assert_eq!(kinds(&events), [Event::Ignored]);
        assert!(!button.is_armed());
    }

    #[test]
    fn a_cat_cannot_feed_itself_by_tapping() {
        // The failure mode this module exists to prevent: many presses, no
        // food. Forty taps over a minute produce forty nothings.
        let mut button = Button::new();
        let mut events = alloc::vec![];

        for i in 0..40 {
            press(&mut button, 1_000 + i * 1_500, 90, &mut events);
        }

        assert!(events.iter().all(|(_, e)| *e == Event::Ignored));
        assert!(!button.is_armed());
    }

    #[test]
    fn a_long_hold_arms_while_still_held() {
        // Not on release: the LED has to start blinking while your finger is
        // still down, or there is no way to know when to let go.
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 1_000, 2_500, &mut events);

        assert_eq!(kinds(&events), [Event::Armed]);
        assert!(events[0].0 < 1_000 + 2_500, "armed only on release");
        assert!(button.is_armed());
    }

    #[test]
    fn the_hold_that_arms_is_not_also_a_feed() {
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 3_000, &mut events);

        assert_eq!(kinds(&events), [Event::Armed]);
    }

    #[test]
    fn arming_then_tapping_feeds() {
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 2_200, &mut events);
        idle(&mut button, 2_200, 3_000, &mut events);
        press(&mut button, 3_000, 80, &mut events);

        assert_eq!(kinds(&events), [Event::Armed, Event::Feed]);
    }

    #[test]
    fn three_portions_is_three_taps_not_three_arms() {
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 2_100, &mut events);
        for i in 0..3 {
            let at = 3_000 + i * 1_200;
            idle(
                &mut button,
                if i == 0 { 2_100 } else { at - 1_200 + 80 },
                at,
                &mut events,
            );
            press(&mut button, at, 80, &mut events);
        }

        assert_eq!(
            kinds(&events),
            [Event::Armed, Event::Feed, Event::Feed, Event::Feed]
        );
    }

    #[test]
    fn feeding_refreshes_the_window() {
        // Otherwise the window would lapse mid-sequence while you are actively
        // using it, which is the most annoying possible moment.
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 2_100, &mut events);

        // A tap at 9 s, well inside the original window.
        idle(&mut button, 2_100, 9_000, &mut events);
        press(&mut button, 9_000, 80, &mut events);

        // The original window would have lapsed at ~12.1 s. Still armed after.
        idle(&mut button, 9_080, 13_000, &mut events);
        assert!(button.is_armed(), "the feed did not refresh the window");

        assert_eq!(kinds(&events), [Event::Armed, Event::Feed]);
    }

    #[test]
    fn the_arm_lapses_on_its_own() {
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 2_100, &mut events);
        idle(&mut button, 2_100, 20_000, &mut events);

        assert_eq!(kinds(&events), [Event::Armed, Event::Expired]);
        assert!(!button.is_armed());

        // And a tap afterwards is locked again, not a feed.
        press(&mut button, 20_000, 80, &mut events);
        assert_eq!(events.last().unwrap().1, Event::Ignored);
    }

    #[test]
    fn it_lapses_exactly_once() {
        // A repeating Expired would have the LED restarting its pattern every
        // poll, and the log line every 25 ms.
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 2_100, &mut events);
        idle(&mut button, 2_100, 60_000, &mut events);

        assert_eq!(
            events.iter().filter(|(_, e)| *e == Event::Expired).count(),
            1
        );
    }

    // --- a hold locks again --------------------------------------------------

    #[test]
    fn a_hold_while_armed_locks_again() {
        let mut b = Button::new();
        let mut events = vec![];

        press(&mut b, 0, ARM_HOLD_MS + 100, &mut events);
        assert!(b.is_armed(), "the first hold arms");

        // A second, separate hold — the release in between is what makes it a
        // different press.
        press(&mut b, 5_000, ARM_HOLD_MS + 100, &mut events);

        assert_eq!(kinds(&events), [Event::Armed, Event::Locked]);
        assert!(!b.is_armed(), "the second hold locks");
    }

    /// The rule that makes the toggle usable rather than baffling.
    ///
    /// Arming fires *while* the button is still held, so without a per-press
    /// latch a single four-second hold would arm at two seconds and lock at
    /// four — a gesture that visibly does nothing.
    #[test]
    fn one_long_hold_arms_once_and_does_not_also_lock() {
        let mut b = Button::new();
        let mut events = vec![];

        press(&mut b, 0, ARM_HOLD_MS * 3, &mut events);

        assert_eq!(kinds(&events), [Event::Armed]);
        assert!(b.is_armed(), "still armed after letting go");
    }

    /// Locking by hold must not also be read as a tap on the way out.
    #[test]
    fn the_hold_that_locks_is_not_also_a_feed() {
        let mut b = Button::new();
        let mut events = vec![];

        press(&mut b, 0, ARM_HOLD_MS + 50, &mut events);
        press(&mut b, 4_000, ARM_HOLD_MS + 50, &mut events);

        assert!(
            !events.iter().any(|(_, e)| *e == Event::Feed),
            "a lock hold fed something: {events:?}"
        );
    }

    /// After locking by hold, the button behaves exactly as if it had lapsed.
    #[test]
    fn a_tap_after_locking_by_hold_is_ignored_again() {
        let mut b = Button::new();
        let mut events = vec![];

        press(&mut b, 0, ARM_HOLD_MS + 50, &mut events);
        press(&mut b, 4_000, ARM_HOLD_MS + 50, &mut events);
        events.clear();
        press(&mut b, 9_000, 40, &mut events);

        assert_eq!(kinds(&events), [Event::Ignored]);
    }

    /// Locking by hold retires the window, so no `Expired` arrives later to
    /// contradict it.
    #[test]
    fn locking_by_hold_cancels_the_pending_expiry() {
        let mut b = Button::new();
        let mut events = vec![];

        press(&mut b, 0, ARM_HOLD_MS + 50, &mut events);
        press(&mut b, 4_000, ARM_HOLD_MS + 50, &mut events);
        events.clear();

        idle(&mut b, 6_000, 40_000, &mut events);

        assert!(
            events.is_empty(),
            "something fired after a deliberate lock: {events:?}"
        );
    }

    #[test]
    fn a_hold_starting_as_the_window_lapses_still_arms() {
        // The ordering inside `poll`. Reporting the lapse first would drop a
        // press that the user had already begun.
        let mut button = Button::new();
        let mut events = alloc::vec![];

        press(&mut button, 0, 2_100, &mut events);
        // Window lapses at 12_100. Start holding just before.
        idle(&mut button, 2_100, 11_500, &mut events);
        press(&mut button, 11_500, 2_100, &mut events);

        let kinds = kinds(&events);
        assert!(kinds.contains(&Event::Armed));
        assert_eq!(
            kinds.iter().filter(|e| **e == Event::Armed).count(),
            2,
            "the second hold should arm too"
        );
        assert!(button.is_armed());
    }

    #[test]
    fn holding_at_boot_needs_the_full_duration() {
        let interval = 50;
        let n = (BOOT_RESET_HOLD_MS / interval) as usize;

        assert!(held_at_boot(core::iter::repeat_n(true, n), interval));
        assert!(!held_at_boot(core::iter::repeat_n(true, n - 1), interval));
    }

    #[test]
    fn letting_go_at_boot_cancels_it_entirely() {
        // Two separate touches must not add up to a wipe.
        let interval = 50;
        let n = (BOOT_RESET_HOLD_MS / interval) as usize;

        let half_released_half: alloc::vec::Vec<bool> = core::iter::repeat_n(true, n / 2)
            .chain(core::iter::once(false))
            .chain(core::iter::repeat_n(true, n))
            .collect();

        assert!(!held_at_boot(half_released_half, interval));
    }

    #[test]
    fn a_button_nobody_touched_at_boot_is_not_a_reset() {
        assert!(!held_at_boot(core::iter::repeat_n(false, 100), 50));
    }
}
