//! What the tasks share, and nothing else.
//!
//! One [`Bus`] static lives in `main.rs` and every task gets a reference to it.
//! That keeps the wiring in one place and out of each task's signature, and it
//! makes the direction of every piece of shared state explicit below.
//!
//! ```text
//!   mqtt      --feed-->  feeder          (portion requests)
//!   schedule  --feed-->  feeder
//!   feeder    --status-> mqtt            (feeding, jammed)
//!   mqtt      --paused-> schedule
//!   mqtt      --time---> schedule
//!   mqtt      --schedule schedule
//!   schedule  --last_fed mqtt
//! ```

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_sync::signal::Signal;

use crate::schedule::{Schedule, TimeSource, Wall};

/// How many unread feed **requests** can be waiting before producers drop them.
///
/// Requests, not portions: one `feed 3` occupies one of these. Nothing to do
/// with `schedule::MAX_SLOTS`, which is meals per day and happens to be the
/// same number, nor with `portions::MAX_PORTIONS`, which caps a single meal.
///
/// Bounded on purpose: producers use `try_send`, so a stuck automation can
/// never block the MQTT or schedule task waiting for room.
pub const FEED_DEPTH: usize = 8;

/// Portion requests, from every producer to the one task that owns the motor.
pub type FeedChannel = Channel<CriticalSectionRawMutex, u8, FEED_DEPTH>;

/// A producer's end of [`FeedChannel`].
pub type FeedSender = Sender<'static, CriticalSectionRawMutex, u8, FEED_DEPTH>;

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

/// When this unit last dispensed a scheduled meal, for the state payload.
///
/// Scheduled feeds only. A manual feed arrives at the feeder task, which has no
/// clock and cannot stamp it; Home Assistant already records button presses in
/// its own history, so duplicating them here would buy nothing.
///
/// Recorded when the request is queued rather than when the hub finishes
/// turning, because a jam discards whatever is pending and there is no moment
/// afterwards that means "done".
pub struct LastFed(Mutex<CriticalSectionRawMutex, Cell<Option<Wall>>>);

impl Default for LastFed {
    fn default() -> Self {
        Self::new()
    }
}

impl LastFed {
    pub const fn new() -> Self {
        Self(Mutex::new(Cell::new(None)))
    }

    pub fn set(&self, at: Wall) {
        self.0.lock(|slot| slot.set(Some(at)));
    }

    pub fn get(&self) -> Option<Wall> {
        self.0.lock(Cell::get)
    }
}

/// Everything the tasks share.
pub struct Bus {
    /// Portion requests. Written by `mqtt` and `schedule`, drained by `feeder`.
    pub feed: FeedChannel,
    /// Written by `feeder`, read by `mqtt`.
    pub status: FeederStatus,
    /// Written by `mqtt` from the retained `paused` topic, read by `schedule`.
    ///
    /// It outlives a broker connection on purpose: the retained flag is replayed
    /// on every reconnect, but until it arrives the last value this unit acted
    /// on is a better answer than `false`.
    pub paused: AtomicBool,
    /// Latest `feeder/time`, `mqtt` to `schedule`.
    ///
    /// A [`Signal`] rather than a channel because only the newest matters: an
    /// old time message is worse than none, and both topics are retained, so
    /// missing one costs nothing.
    pub time: Signal<CriticalSectionRawMutex, TimeSync>,
    /// Latest `feeder/schedule`, `mqtt` to `schedule`.
    pub schedule: Signal<CriticalSectionRawMutex, Schedule>,
    /// Written by `schedule`, read by `mqtt`.
    pub last_fed: LastFed,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub const fn new() -> Self {
        Self {
            feed: Channel::new(),
            status: FeederStatus::new(),
            paused: AtomicBool::new(false),
            time: Signal::new(),
            schedule: Signal::new(),
            last_fed: LastFed::new(),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
}
