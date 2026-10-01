//! What the unit tells Home Assistant happened: a meal served, a meal skipped,
//! a portion fed by hand at the unit, a jam.
//!
//! Pure logic. A task that sees one of these queues it on `Bus::events`, and
//! `mqtt.rs` publishes it to `feeder/<id>/event`, **not retained**, where the
//! discovered `event` entity (`discovery::Entity::Feeding`) turns each one
//! into a line in Home Assistant's Activity. The payload is the event
//! platform's: an `event_type` from [`EVENT_TYPES`], and every other key
//! becomes an attribute of that event.
//!
//! | `event_type` | Raised by | Attributes |
//! |---|---|---|
//! | `scheduled` | `schedule`, when a slot falls due and is queued | `portions`, `slot` |
//! | `skipped` | `schedule`, when a slot is resolved without feeding | `slot`, `reason` |
//! | `manual` | `ui` (the knob) and `web` (the admin page) | `portions`, `source` |
//! | `jammed` | `feeder`, when the jam budget runs out | |
//!
//! **A feed from Home Assistant raises nothing.** Activity already records the
//! button press or the script that sent it, so an event as well would show it
//! twice. The `manual` events exist for exactly the feeds Home Assistant
//! cannot otherwise see.
//!
//! **`scheduled` means queued, not finished**, the same moment `last_fed` is
//! recorded. A meal that then jams is followed by a `jammed` event, which is
//! what makes the two distinguishable in the log.
//!
//! **Every event carries `at`**, the unit's trusted time when it happened,
//! when it has one. An event raised while the broker is down waits in the
//! queue and reaches Home Assistant late, stamped with when it *arrived*;
//! `at` is the time it actually happened.

use core::fmt::Write as _;

use heapless::String;

use crate::schedule::{Skipped, Wall};

/// Every `event_type` this unit can send, as the discovery config announces
/// them. Home Assistant drops an event whose type is not in this list.
pub const EVENT_TYPES: [&str; 4] = ["scheduled", "skipped", "manual", "jammed"];

/// The longest payload is a `scheduled` event with a three-digit count, a
/// slot and an offset time, at about 100 bytes; a test measures each kind.
pub const EVENT_LEN: usize = 128;

/// Which control at the unit fed by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Knob,
    Web,
}

/// Something worth a line in Home Assistant's Activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A slot fell due and its portions were queued.
    Scheduled { minute_of_day: u16, portions: u8 },
    /// A slot was resolved without feeding. Only the reasons a person would
    /// want to read about: the boot baseline is not one, see [`Event::skipped`].
    Skipped { minute_of_day: u16, why: Skipped },
    /// Portions fed at the unit itself.
    Manual { source: Source, portions: u8 },
    /// No click within the jam budget; the motor stopped and pending portions
    /// were discarded.
    Jammed,
}

impl Event {
    /// The event for a slot the scheduler resolved without feeding, if it is
    /// one worth reporting. The baseline pass at boot marks every slot already
    /// past as consumed, which is bookkeeping rather than a missed meal.
    pub fn skipped(minute_of_day: u16, why: Skipped) -> Option<Self> {
        match why {
            Skipped::Baseline => None,
            Skipped::Paused | Skipped::TooLate { .. } => Some(Self::Skipped { minute_of_day, why }),
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Scheduled { .. } => "scheduled",
            Self::Skipped { .. } => "skipped",
            Self::Manual { .. } => "manual",
            Self::Jammed => "jammed",
        }
    }

    /// The payload for `feeder/<id>/event`, with `at` when the unit had a
    /// trusted time to give it.
    pub fn to_json(&self, at: Option<Wall>) -> String<EVENT_LEN> {
        let mut json = String::new();
        // Every write fits: the test below renders the longest of each kind.
        let _ = self.write_json(&mut json, at);
        json
    }

    fn write_json(&self, json: &mut String<EVENT_LEN>, at: Option<Wall>) -> core::fmt::Result {
        write!(json, r#"{{"event_type":"{}""#, self.event_type())?;
        match *self {
            Self::Scheduled {
                minute_of_day,
                portions,
            } => {
                write!(json, r#","portions":{portions}"#)?;
                write_slot(json, minute_of_day)?;
            }
            Self::Skipped { minute_of_day, why } => {
                write_slot(json, minute_of_day)?;
                let reason = match why {
                    Skipped::Paused => "paused",
                    // Never constructed, see `Event::skipped`; named anyway so
                    // the match stays exhaustive without a catch-all.
                    Skipped::Baseline => "baseline",
                    Skipped::TooLate { .. } => "late",
                };
                write!(json, r#","reason":"{reason}""#)?;
            }
            Self::Manual { source, portions } => {
                let source = match source {
                    Source::Knob => "knob",
                    Source::Web => "web",
                };
                write!(json, r#","portions":{portions},"source":"{source}""#)?;
            }
            Self::Jammed => {}
        }
        if let Some(at) = at {
            write!(json, r#","at":"{at}""#)?;
        }
        write!(json, "}}")
    }
}

/// `"slot":"HH:MM"`, the way a schedule names its meals.
fn write_slot(json: &mut String<EVENT_LEN>, minute_of_day: u16) -> core::fmt::Result {
    write!(
        json,
        r#","slot":"{:02}:{:02}""#,
        minute_of_day / 60,
        minute_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Date;

    fn at() -> Wall {
        Wall {
            date: Date {
                year: 2026,
                month: 10,
                day: 1,
            },
            second_of_day: 8 * 3600,
            offset_minutes: Some(-570),
        }
    }

    /// One of every kind, at its widest.
    fn every_kind() -> [Event; 6] {
        [
            Event::Scheduled {
                minute_of_day: 23 * 60 + 59,
                portions: u8::MAX,
            },
            Event::Skipped {
                minute_of_day: 23 * 60 + 59,
                why: Skipped::Paused,
            },
            Event::Skipped {
                minute_of_day: 0,
                why: Skipped::TooLate { by_s: u32::MAX },
            },
            Event::Manual {
                source: Source::Knob,
                portions: u8::MAX,
            },
            Event::Manual {
                source: Source::Web,
                portions: 1,
            },
            Event::Jammed,
        ]
    }

    fn json(event: Event, at: Option<Wall>) -> serde_json::Value {
        let payload = event.to_json(at);
        serde_json::from_str(&payload).unwrap_or_else(|e| panic!("{event:?}: {e}\n{payload}"))
    }

    #[test]
    fn every_event_is_json_with_an_announced_type_and_fits() {
        for event in every_kind() {
            for when in [Some(at()), None] {
                let payload = event.to_json(when);
                assert!(payload.ends_with('}'), "{event:?} was cut short: {payload}");
                let value = json(event, when);
                assert!(
                    EVENT_TYPES.contains(&value["event_type"].as_str().unwrap()),
                    "{event:?}"
                );
            }
        }
    }

    #[test]
    fn a_scheduled_meal_names_its_slot_and_portions() {
        let value = json(
            Event::Scheduled {
                minute_of_day: 8 * 60 + 5,
                portions: 2,
            },
            Some(at()),
        );
        assert_eq!(value["event_type"], "scheduled");
        assert_eq!(value["slot"], "08:05");
        assert_eq!(value["portions"], 2);
        assert_eq!(value["at"], "2026-10-01T08:00:00-09:30");
    }

    #[test]
    fn a_skipped_meal_says_why() {
        let paused = json(Event::skipped(19 * 60, Skipped::Paused).unwrap(), None);
        assert_eq!(paused["reason"], "paused");
        assert_eq!(paused["slot"], "19:00");

        let late = json(
            Event::skipped(19 * 60, Skipped::TooLate { by_s: 600 }).unwrap(),
            None,
        );
        assert_eq!(late["reason"], "late");
    }

    /// The boot baseline marks past slots consumed; that is not a missed meal,
    /// and reporting it would add a "skipped" line for every meal of the day
    /// at every reboot.
    #[test]
    fn the_boot_baseline_is_not_reported() {
        assert_eq!(Event::skipped(8 * 60, Skipped::Baseline), None);
    }

    #[test]
    fn a_manual_feed_names_its_source() {
        let value = json(
            Event::Manual {
                source: Source::Web,
                portions: 3,
            },
            None,
        );
        assert_eq!(value["source"], "web");
        assert_eq!(value["portions"], 3);
    }

    #[test]
    fn without_a_trusted_time_there_is_no_at() {
        assert!(json(Event::Jammed, None).get("at").is_none());
        assert_eq!(json(Event::Jammed, None)["event_type"], "jammed");
    }
}
