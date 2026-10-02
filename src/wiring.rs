//! What the tasks share, and nothing else.
//!
//! One [`Bus`] static lives in `main.rs` and every task gets a reference to it.
//! That keeps the wiring in one place and out of each task's signature, and it
//! makes the direction of every piece of shared state explicit below.
//!
//! ```text
//!   mqtt      --feed-->  feeder          (portion requests)
//!   schedule  --feed-->  feeder
//!   ui, web   --feed-->  feeder
//!   schedule  --events-> mqtt            (a meal served or skipped)
//!   ui, web   --events-> mqtt            (a feed at the unit)
//!   feeder    --events-> mqtt            (a jam)
//!   feeder    --status-> mqtt            (feeding, jammed)
//!   ui, web, mqtt --paused-> schedule (stored first: the unit owns it)
//!   mqtt      --time---> schedule
//!   rtc       --time---> schedule     (at boot, if the DS3231 kept time)
//!   ui        --time---> schedule     (set by hand on the knob)
//!   mqtt      --rtc_time rtc          (live times, to keep the DS3231 set)
//!   ui        --rtc_time rtc          (a hand-set time, likewise)
//!   mqtt      --schedule schedule     (a schedule or a slot edit; stored, then in force)
//!   schedule  --held---> mqtt, display (what the unit holds; the echo, meals)
//!   schedule  --changed> mqtt         (republish the echo)
//!   schedule  --now----> display, ui  (the trusted time)
//!   schedule  --last_fed mqtt, display
//!   schedule  --next---> display      (the upcoming slot)
//!   ui        --pressed> display      (wakes the panel)
//!   ui        --mode---> display      (which page, or the menu)
//!   ui        --redraw-> display      (now, not at the next tick)
//!   ui, web, mqtt --pause_changed> mqtt (publish the state now)
//!   main      --ip-----> display
//! ```

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_sync::signal::Signal;

use log::warn;

use crate::calibrate::{Failure, Measurement, Progress};
use crate::events::Event;
use crate::indicator::Health;
use crate::menu::{Calibration, Mode};
use crate::schedule::{Schedule, ScheduleCommand, Slot, TimeSource, Wall};
use crate::store::{Store, StoreError};
use crate::tz::Zone;

/// How many unread feed **requests** can be waiting before producers drop them.
///
/// Requests, not portions: one `feed 3` occupies one of these. Nothing to do
/// with `schedule::MAX_SLOTS`, which is meals per day and happens to be the
/// same number, nor with `portions::MAX_CLICKS`, which caps a single meal.
///
/// Bounded on purpose: producers use `try_send`, so a stuck automation can
/// never block the MQTT or schedule task waiting for room.
pub const FEED_DEPTH: usize = 8;

/// How many events can wait for the broker. Eight covers a day's meals and a
/// jam through an outage of several hours; past that the newest are dropped,
/// which costs a line in Home Assistant's Activity and nothing else.
pub const EVENT_DEPTH: usize = 8;

/// Portion requests, from every producer to the one task that owns the motor.
pub type FeedChannel = Channel<CriticalSectionRawMutex, u8, FEED_DEPTH>;

/// A producer's end of [`FeedChannel`].
pub type FeedSender = Sender<'static, CriticalSectionRawMutex, u8, FEED_DEPTH>;

/// How many schedule commands can wait for the schedule task, which drains
/// them once a second.
///
/// Twice the sixteen `Meal n` entities would need: a Home Assistant script
/// setting every one of them lands inside that second, and a command that
/// finds no room is dropped with a warning — an edit silently lost.
pub const SCHEDULE_DEPTH: usize = 2 * 2 * crate::schedule::MAX_SLOTS;

/// Milliseconds since boot. The one clock every task measures against.
pub fn now_ms() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
}

/// A `feeder/time` message, with the monotonic reading taken as it arrived.
///
/// The stamp travels with the message rather than being taken when the
/// schedule task gets round to it, so a busy executor costs accuracy nowhere.
///
/// `source` carries MQTT's retained-or-live distinction through to the clock,
/// which is what stops a unit trusting a `feeder/time` that Home Assistant
/// stopped refreshing hours ago. See [`crate::schedule::LocalClock`].
#[derive(Debug, Clone, Copy)]
pub struct TimeSync {
    pub monotonic_ms: u64,
    pub wall: Wall,
    pub source: TimeSource,
}

/// What the feeder task knows about itself, published by `mqtt`.
///
/// Two booleans read far more often than they are written, from a task that
/// must never block, so this is atomics rather than a mutex. `Relaxed` is
/// enough: each flag is read on its own and nothing is ordered against it.
///
/// The MQTT task must report the flag the feeder is *acting on*, never echo a
/// command back as it arrives — an echo makes Home Assistant look correct even
/// when the feeder never saw the change.
pub struct FeederStatus {
    feeding: AtomicBool,
    jammed: AtomicBool,
}

impl Default for FeederStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl FeederStatus {
    pub const fn new() -> Self {
        Self {
            feeding: AtomicBool::new(false),
            jammed: AtomicBool::new(false),
        }
    }

    pub fn set(&self, feeding: bool, jammed: bool) {
        self.feeding.store(feeding, Ordering::Relaxed);
        self.jammed.store(jammed, Ordering::Relaxed);
    }

    pub fn feeding(&self) -> bool {
        self.feeding.load(Ordering::Relaxed)
    }

    pub fn jammed(&self) -> bool {
        self.jammed.load(Ordering::Relaxed)
    }
}

/// How far the unit has got towards being useful, for the LED.
///
/// Three facts that each used to live inside the one task that knew them:
/// association in `wifi_task`, the broker connection inside `mqtt::run`, and
/// clock trust inside `schedule_task`'s `LocalClock`. None of them could be
/// observed from anywhere else, which is exactly why the states they describe
/// were invisible without a serial console.
///
/// Atomics rather than a mutex, for the same reason as [`FeederStatus`]: read
/// far more often than written, from tasks that must not block. `Relaxed` is
/// enough — each flag is read on its own and nothing is ordered against it.
///
/// Deliberately **not** one shared value. Three separate flags mean three
/// writers can never race to describe the same field, and the reader combines
/// them in [`crate::indicator::Status::of`] where the priority is written down
/// and tested.
pub struct Connectivity {
    link: AtomicBool,
    broker: AtomicBool,
    armed: AtomicBool,
}

impl Default for Connectivity {
    fn default() -> Self {
        Self::new()
    }
}

impl Connectivity {
    pub const fn new() -> Self {
        Self {
            link: AtomicBool::new(false),
            broker: AtomicBool::new(false),
            armed: AtomicBool::new(false),
        }
    }

    /// Associated with the Wi-Fi network. Written by `wifi_task`.
    pub fn set_link(&self, up: bool) {
        self.link.store(up, Ordering::Relaxed);

        // Losing the network necessarily loses the broker, and the MQTT task
        // may take a while to notice. Clearing it here keeps the LED from
        // reporting the second fault when the first one is the real answer.
        if !up {
            self.broker.store(false, Ordering::Relaxed);
        }
    }

    /// Connected to the broker. Written by `mqtt`.
    pub fn set_broker(&self, up: bool) {
        self.broker.store(up, Ordering::Relaxed);
    }

    /// The schedule is armed, i.e. the clock has had a *live* time. Written by
    /// `schedule_task`.
    ///
    /// Not "has a time": a retained one starts the clock but leaves the
    /// schedule holding, and a unit in that state will not feed. That is the
    /// distinction the LED exists to make visible.
    pub fn set_armed(&self, armed: bool) {
        self.armed.store(armed, Ordering::Relaxed);
    }

    pub fn link(&self) -> bool {
        self.link.load(Ordering::Relaxed)
    }

    pub fn broker(&self) -> bool {
        self.broker.load(Ordering::Relaxed)
    }

    pub fn armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }
}

/// When this unit last dispensed a scheduled meal, for the state payload.
///
/// Scheduled feeds only. A manual feed arrives at the feeder task, which has no
/// clock and cannot stamp it; Home Assistant already records button presses in
/// its own history, so duplicating them here would buy nothing.
///
/// Recorded when the request is queued rather than when the hub finishes
/// turning, because a jam discards whatever is pending and there is no moment
/// afterwards that means "done".
///
/// Carries the **portion count** alongside the time, because the display shows
/// both and "fed at 08:00" without a quantity answers half the question
/// somebody standing at a feeder is asking.
///
/// Portions as requested, never clicks. `portions::clicks_for` runs downstream
/// of this, so a unit with a portion scale would otherwise report a number that
/// matches neither the schedule that asked nor Home Assistant's history.
pub struct LastFed(Mutex<CriticalSectionRawMutex, Cell<Option<(Wall, u8)>>>);

impl Default for LastFed {
    fn default() -> Self {
        Self::new()
    }
}

impl LastFed {
    pub const fn new() -> Self {
        Self(Mutex::new(Cell::new(None)))
    }

    pub fn set(&self, at: Wall, portions: u8) {
        self.0.lock(|slot| slot.set(Some((at, portions))));
    }

    pub fn get(&self) -> Option<(Wall, u8)> {
        self.0.lock(Cell::get)
    }
}

/// The slot this unit will feed next, or `None` when it cannot say.
///
/// Written by `schedule`, read by `display`. It is carried across rather than
/// queried because `Scheduler` belongs to the schedule task — and asking it is
/// not free: `next_due` marks a slot consumed before it answers, so a reader
/// after a screenful of text would eat the meal. `Scheduler::upcoming` is the
/// `&self` answer, and this is where it lands.
///
/// `None` means the schedule genuinely has nothing to say: no slots, or no
/// trusted time to measure "next" against. It does **not** mean paused — a
/// paused unit still has an upcoming slot it will not feed, and suppressing
/// that is the display's decision, not this one's.
pub struct NextSlot(Mutex<CriticalSectionRawMutex, Cell<Option<Slot>>>);

impl Default for NextSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl NextSlot {
    pub const fn new() -> Self {
        Self(Mutex::new(Cell::new(None)))
    }

    pub fn set(&self, slot: Option<Slot>) {
        self.0.lock(|cell| cell.set(slot));
    }

    pub fn get(&self) -> Option<Slot> {
        self.0.lock(Cell::get)
    }
}

/// When the outside button was last pressed, for waking the screen.
///
/// **Every press**, not only the ones the gesture machine makes something of.
/// Waking the panel is not a gesture and must not compete with one: a tap while
/// locked still does nothing to the feeder, and now lights the screen, which is
/// about the most useful thing an ignored tap could do.
///
/// A mutex rather than an atomic because this is a `u64` and **there is no
/// `AtomicU64` on 32-bit RISC-V**. An `AtomicU32` of milliseconds would wrap
/// every 49 days and wake or blank the panel once per wrap — the same trap
/// `indicator.rs` documents for its own one-shot timing. Uncontended, and read
/// once a second, so the lock costs nothing worth measuring.
pub struct LastPress(Mutex<CriticalSectionRawMutex, Cell<Option<u64>>>);

impl Default for LastPress {
    fn default() -> Self {
        Self::new()
    }
}

impl LastPress {
    pub const fn new() -> Self {
        Self(Mutex::new(Cell::new(None)))
    }

    pub fn set(&self, at_ms: u64) {
        self.0.lock(|cell| cell.set(Some(at_ms)));
    }

    /// `None` means the button has not been touched since boot, which is why
    /// the panel starts dark.
    pub fn get(&self) -> Option<u64> {
        self.0.lock(Cell::get)
    }
}

/// A small `Copy` value behind a lock, for state one task writes and another
/// samples. Same shape as [`LastPress`], without its commentary.
pub struct Shared<T: Copy>(Mutex<CriticalSectionRawMutex, Cell<T>>);

impl<T: Copy> Shared<T> {
    pub const fn new(value: T) -> Self {
        Self(Mutex::new(Cell::new(value)))
    }

    pub fn set(&self, value: T) {
        self.0.lock(|cell| cell.set(value));
    }

    pub fn get(&self) -> T {
        self.0.lock(Cell::get)
    }
}

/// Something held for others to read, or `None` if there is nothing yet.
///
/// For values that are not `Copy` — a schedule, a timezone — so this is a lock
/// around a `RefCell` rather than a [`Shared`]. Read by cloning: each is a few
/// dozen bytes.
pub struct Held<T>(Mutex<CriticalSectionRawMutex, RefCell<Option<T>>>);

impl<T: Clone> Default for Held<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Held<T> {
    pub const fn new() -> Self {
        Self(Mutex::new(RefCell::new(None)))
    }

    pub fn set(&self, value: Option<T>) {
        self.0.lock(|cell| *cell.borrow_mut() = value);
    }

    pub fn get(&self) -> Option<T> {
        self.0.lock(|cell| cell.borrow().clone())
    }
}

/// The schedule the unit holds, or `None` if it has never been given one.
pub type HeldSchedule = Held<Schedule>;

impl Held<Schedule> {
    /// How many meals a day will feed, `0` when there is no schedule at all
    /// or every slot is switched off — the same thing to anyone asking whether
    /// this unit will feed.
    pub fn meals(&self) -> usize {
        self.0
            .lock(|cell| cell.borrow().as_ref().map_or(0, Schedule::meals))
    }
}

/// Everything the tasks share.
pub struct Bus {
    /// Portion requests. Written by `mqtt`, `schedule`, `ui` and `web`,
    /// drained by `feeder`.
    pub feed: FeedChannel,
    /// Things worth a line in Home Assistant's Activity, each with the trusted
    /// time it happened at, if any. Written through [`Bus::report`] by
    /// `schedule`, `ui`, `web` and `feeder`; drained by `mqtt` onto
    /// `feeder/<id>/event`.
    ///
    /// A queue, not a [`Signal`]: two meals or a meal and its jam are two
    /// lines, both wanted. It keeps them while the broker is unreachable.
    pub events: Channel<CriticalSectionRawMutex, (Event, Option<Wall>), EVENT_DEPTH>,
    /// Written by `feeder`, read by `mqtt`.
    pub status: FeederStatus,
    /// Whether the schedule is paused. Seeded by `main` from flash, then
    /// changed only through [`Bus::set_pause`], by `ui`, `web` and a live
    /// command on `mqtt`; read by `schedule`. The unit is the authority
    /// (ADR-0023), so nothing from the broker overrides it on reconnect.
    paused: AtomicBool,
    /// The latest time for the schedule's clock: `feeder/time` from `mqtt`,
    /// the DS3231 from `rtc` at boot, or a hand-set time from `ui`. Each
    /// carries its [`TimeSource`](crate::schedule::TimeSource), which decides
    /// how far it is trusted.
    ///
    /// A [`Signal`] rather than a channel because only the newest matters: an
    /// old time is worse than none, and `feeder/time` is republished every
    /// minute, so missing one costs nothing.
    pub time: Signal<CriticalSectionRawMutex, TimeSync>,
    /// Latest *live* `feeder/time`, `mqtt` to `rtc`, which sets the DS3231
    /// from it. Live only: a retained time can be any age, and writing one
    /// into the RTC would launder a stale time into one that looks set.
    pub rtc_time: Signal<CriticalSectionRawMutex, TimeSync>,
    /// A schedule command just received — a whole schedule, or one slot
    /// edited from Home Assistant — `mqtt` to `schedule`, which applies it,
    /// stores the result in flash and puts it in force.
    ///
    /// A queue, not a [`Signal`]: a signal keeps only the newest value, and a
    /// meal's time and its portions sent a moment apart are two edits, both
    /// wanted.
    pub schedule: Channel<CriticalSectionRawMutex, ScheduleCommand, SCHEDULE_DEPTH>,
    /// The schedule this unit holds. Written by `schedule` — from flash at
    /// boot, then on every command it stores — and read by `mqtt` for the
    /// retained echo and the state payload's `meals`, and by `display`.
    pub held: HeldSchedule,
    /// The timezone this unit follows when nobody publishes the time, or `None`
    /// for plain local time. Written by `main` from flash at boot and by
    /// `web`; read by `schedule` every tick and by `web` for the page.
    pub zone: Held<Zone>,
    /// The held schedule changed: `schedule` to `mqtt`, which republishes the
    /// retained `feeder/<id>/schedule/state` echo.
    pub schedule_changed: Signal<CriticalSectionRawMutex, ()>,
    /// Written by `schedule`, read by `mqtt`.
    pub last_fed: LastFed,
    /// Written by `schedule`, read by `display`.
    pub next: NextSlot,
    /// Written by `ui`, read by `display`. Every press and every turn.
    pub last_press: LastPress,
    /// Which page is up, or the menu and its cursor. Written by `ui`, read by
    /// `display`.
    pub mode: Shared<Mode>,
    /// Redraw now. Signalled by `ui` after every input, so the screen follows
    /// the knob rather than lagging it by up to a second.
    pub redraw: Signal<CriticalSectionRawMutex, ()>,
    /// The pause changed: [`Bus::set_pause`] to `mqtt`, which publishes the
    /// state payload now rather than at the next interval, so Home Assistant's
    /// switch — not optimistic — follows at once.
    pub pause_changed: Signal<CriticalSectionRawMutex, ()>,
    /// This unit's address, once DHCP has handed one out. Written by `main`,
    /// read by `display`.
    pub ip: Shared<Option<[u8; 4]>>,
    /// The calibration in force. Seeded by `main` from the record, rewritten
    /// by `ui` when the knob saves a new figure; read by `feeder`, which
    /// applies it at its next idle moment, and by `display`.
    pub calibration: Shared<Calibration>,
    /// Start a calibration run: `ui` to `feeder`, which runs it only when idle.
    pub calibrate: Signal<CriticalSectionRawMutex, ()>,
    /// Clicks counted so far in a calibration run: `feeder` to `ui`.
    pub calibration_clicks: Signal<CriticalSectionRawMutex, u8>,
    /// How a calibration run ended: `feeder` to `ui`.
    pub calibration_result: Signal<CriticalSectionRawMutex, Result<Measurement, Failure>>,
    /// Where the latest calibration run is, whoever started it. Written by
    /// `feeder`, read by `web`, which cannot share the two signals above with
    /// `ui` — a signal has one reader.
    pub calibration_progress: Shared<Progress>,
    /// The time now, only while the clock is trusted. Written by `schedule`
    /// each tick, read by `display` for the home page and by `ui` so the
    /// knob's clock editor opens on it.
    pub now: Shared<Option<Wall>>,
    /// Written by `wifi`, `mqtt` and `schedule`, read by `indicator`.
    pub net: Connectivity,
    /// This unit is in setup mode, serving its own network.
    ///
    /// Set once, by the boot path, and never cleared: setup mode is left by
    /// rebooting, not by changing its mind.
    pub setup: AtomicBool,
    /// The menu is open. Written by `ui`, read by `indicator`.
    ///
    /// The gesture state itself stays inside the ui task — this is only the
    /// one bit the LED needs, so nothing else can reach in and change what a
    /// press means.
    pub button_armed: AtomicBool,
    /// The BOOT button is being held towards a reset. Written by `reset`,
    /// read by `indicator`.
    pub reset_held: AtomicBool,
    /// An upload is writing the idle app slot. Written by `web`; `feeder`
    /// starts no turn while it is set, because a flash erase stalls whatever
    /// runs from flash and a click could be acted on late (ADR-0022).
    pub flash_busy: AtomicBool,
    /// `feeder` is owed portions it did not start because of `flash_busy`.
    /// Read by `web`, which restarts into new firmware only once they ran.
    pub feed_held: AtomicBool,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    /// Queues `event` for Home Assistant, stamped with the trusted time now.
    /// Never blocks: a full queue drops it with a warning, because no feed or
    /// jam may wait on the broker.
    pub fn report(&self, event: Event) {
        if self.events.try_send((event, self.now.get())).is_err() {
            warn!("events: queue full, {} dropped", event.event_type());
        }
    }

    pub const fn new() -> Self {
        Self {
            feed: Channel::new(),
            events: Channel::new(),
            status: FeederStatus::new(),
            paused: AtomicBool::new(false),
            time: Signal::new(),
            rtc_time: Signal::new(),
            schedule: Channel::new(),
            held: HeldSchedule::new(),
            zone: Held::new(),
            schedule_changed: Signal::new(),
            last_fed: LastFed::new(),
            next: NextSlot::new(),
            last_press: LastPress::new(),
            mode: Shared::new(Mode::Locked {
                page: crate::menu::Page::Home,
            }),
            redraw: Signal::new(),
            pause_changed: Signal::new(),
            ip: Shared::new(None),
            now: Shared::new(None),
            calibrate: Signal::new(),
            calibration_clicks: Signal::new(),
            calibration_result: Signal::new(),
            calibration_progress: Shared::new(Progress::None),
            calibration: Shared::new(Calibration {
                portion_scale_pct: crate::portions::SCALE_UNCHANGED,
                detent_ms: crate::provisioning::DEFAULT_DETENT_MS,
            }),
            net: Connectivity::new(),
            setup: AtomicBool::new(false),
            reset_held: AtomicBool::new(false),
            button_armed: AtomicBool::new(false),
            flash_busy: AtomicBool::new(false),
            feed_held: AtomicBool::new(false),
        }
    }

    /// Everything the LED is allowed to know, sampled in one place.
    ///
    /// Taken as a snapshot rather than read field by field inside the decision,
    /// so the priority ladder cannot see one flag change underneath another and
    /// report a state that never actually existed.
    pub fn health(&self) -> Health {
        Health {
            reset_held: self.reset_held.load(Ordering::Relaxed),
            button_armed: self.button_armed.load(Ordering::Relaxed),
            setup: self.setup.load(Ordering::Relaxed),
            link: self.net.link(),
            broker: self.net.broker(),
            armed: self.net.armed(),
            paused: self.is_paused(),
            feeding: self.status.feeding(),
            jammed: self.status.jammed(),
        }
    }

    /// A time set by a person — on the knob or the admin page — to the
    /// schedule's clock and to the RTC behind it.
    ///
    /// Through the paths a live `feeder/time` takes, stamped now, so there is
    /// no second way for a time to enter the unit. `Manual` behaves as `Live`
    /// does — it arms and it overrides — and the RTC task writes it because it
    /// differs from what the chip holds. Home Assistant's next live time, if
    /// there is one, still has the last word.
    pub fn set_clock_by_hand(&self, wall: Wall) {
        let sync = TimeSync {
            monotonic_ms: now_ms(),
            wall,
            source: TimeSource::Manual,
        };
        self.time.signal(sync);
        self.rtc_time.signal(sync);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// The pause as read from flash at boot. Nothing is written or published.
    pub fn seed_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Pauses or resumes the schedule, from whichever control asked.
    ///
    /// Flash first, then in force, so nothing claims a pause the unit would
    /// forget on a reboot; a failed write changes nothing. An unchanged value
    /// is not rewritten. `Ok(true)` when it changed.
    pub fn set_pause(&self, store: &mut Store, paused: bool) -> Result<bool, StoreError> {
        if self.is_paused() == paused {
            return Ok(false);
        }
        store.save_paused(paused)?;
        self.paused.store(paused, Ordering::Relaxed);
        self.pause_changed.signal(());
        Ok(true)
    }
}
