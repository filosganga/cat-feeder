//! Handles that let tasks talk to each other, and nothing else.
//!
//! The statics themselves live in `main.rs`, which is where the wiring belongs.
//! This module only names the types, so that `mqtt` can hold a feed sender and
//! read the feeder's status without either task knowing about the other.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Sender};

/// How many portion requests can be waiting before producers start dropping.
///
/// Bounded on purpose: producers use `try_send`, so a stuck automation can
/// never block the MQTT or clock task waiting for room.
pub const FEED_DEPTH: usize = 8;

/// Portion requests, from every producer to the one task that owns the motor.
pub type FeedChannel = Channel<CriticalSectionRawMutex, u8, FEED_DEPTH>;

/// A producer's end of [`FeedChannel`].
pub type FeedSender = Sender<'static, CriticalSectionRawMutex, u8, FEED_DEPTH>;

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
