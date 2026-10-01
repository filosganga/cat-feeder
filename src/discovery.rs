//! Home Assistant MQTT discovery: which entities a unit announces, and the
//! retained config each one is announced with.
//!
//! Pure logic. `mqtt.rs` publishes what this renders. It lives apart so the
//! payloads can be checked on the host — parsed as JSON and measured against
//! the buffer — because a malformed or truncated config is the classic reason
//! an entity never appears, and Home Assistant says nothing about it.
//!
//! ## The entities
//!
//! | Entity | Component | Reads | Writes |
//! |---|---|---|---|
//! | Feed | `button` | | `feeder/<id>/feed` |
//! | Paused | `switch` | `feeder/<id>/state` | `feeder/<id>/paused`, retained |
//! | Jammed | `binary_sensor` | `feeder/<id>/state` | |
//! | Feeding | `event` | `feeder/<id>/event` | |
//! | Meal *n* time, ×8 | `time` | `feeder/<id>/schedule/state` | `feeder/<id>/meal/<n>/time` |
//! | Meal *n* portions, ×8 | `number` | `feeder/<id>/schedule/state` | `feeder/<id>/meal/<n>/portions` |
//!
//! **The meal entities are how a schedule is edited with nothing installed** —
//! v2's point 4. They read the same retained echo the unit already publishes,
//! picking their slot out by position, so the unit's flash stays the only copy
//! and Home Assistant only ever shows it back. A position the schedule does not
//! reach reads `None`, which both platforms show as unknown.
//!
//! Their commands are **not retained**, which is Home Assistant's default for
//! both platforms and the rule for every command topic here: the unit refuses a
//! retained one, the same as a retained `feeder/<id>/schedule`. Neither is
//! optimistic — a value changes on the page only once the unit's echo says it
//! took, so an edit the unit refused springs back.

use core::fmt::Write as _;

use heapless::String;

use crate::events::EVENT_TYPES;
use crate::portions::MAX_CLICKS;
use crate::schedule::MAX_SLOTS;

/// The longest topic this renders is a meal's portions config,
/// `homeassistant/number/feeder_<id>/meal_8_portions/config`, 57 characters.
pub const TOPIC_LEN: usize = 64;

/// A config with the device block runs to a little over 500 bytes; the test
/// below measures every one against this.
pub const DISCOVERY_LEN: usize = 640;

/// One entity a unit announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Feed,
    Paused,
    Jammed,
    /// What the unit did by itself — a meal served or skipped, a feed at the
    /// knob or the admin page, a jam — as lines in Home Assistant's Activity.
    /// The payloads are `events.rs`'s.
    Feeding,
    /// Zero-based slot. Shown and addressed one-based.
    MealTime(u8),
    MealPortions(u8),
}

/// A rendered config did not fit its buffer. `mqtt.rs` refuses to publish a
/// truncated one rather than announcing a broken entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong;

impl From<core::fmt::Error> for TooLong {
    fn from(_: core::fmt::Error) -> Self {
        Self
    }
}

/// Every entity, in the order they are announced.
pub fn entities() -> impl Iterator<Item = Entity> {
    let meals = (0..MAX_SLOTS as u8).flat_map(|i| [Entity::MealTime(i), Entity::MealPortions(i)]);
    [
        Entity::Feed,
        Entity::Paused,
        Entity::Jammed,
        Entity::Feeding,
    ]
    .into_iter()
    .chain(meals)
}

/// Repeated verbatim in every payload, closing it. `identifiers` is what
/// joins them into one device, and `model` is a contract with
/// `cat_feeder.yaml`, which finds the units by it.
///
/// `configuration_url` is Home Assistant's *Visit device* link, to the admin
/// page at whatever address the unit had when it connected — `mqtt.rs`
/// refreshes `Bus::ip` at the start of every session, so a new lease is
/// picked up on the next connect. Left out when the address is
/// not known, rather than pointing somewhere wrong.
fn close_with_device(
    payload: &mut String<DISCOVERY_LEN>,
    id: &str,
    sw: &str,
    ip: Option<[u8; 4]>,
) -> Result<(), TooLong> {
    write!(
        payload,
        r#""device":{{"identifiers":["feeder_{id}"],"name":"Cat feeder {id}","#
    )?;
    write!(
        payload,
        r#""manufacturer":"DIY","model":"cat-feeder ESP32-C6","sw_version":"{sw}""#
    )?;
    if let Some([a, b, c, d]) = ip {
        write!(payload, r#","configuration_url":"http://{a}.{b}.{c}.{d}/""#)?;
    }
    write!(payload, "}}}}")?;
    Ok(())
}

impl Entity {
    fn component(self) -> &'static str {
        match self {
            Self::Feed => "button",
            Self::Paused => "switch",
            Self::Jammed => "binary_sensor",
            Self::Feeding => "event",
            Self::MealTime(_) => "time",
            Self::MealPortions(_) => "number",
        }
    }

    /// The retained config's topic and payload, into the caller's buffers.
    ///
    /// All of them carry a `unique_id` and the same `device` block; without
    /// `unique_id` the device block is ignored and the entities appear loose.
    pub fn render(
        self,
        id: &str,
        sw: &str,
        ip: Option<[u8; 4]>,
        topic: &mut String<TOPIC_LEN>,
        payload: &mut String<DISCOVERY_LEN>,
    ) -> Result<(), TooLong> {
        topic.clear();
        payload.clear();

        let component = self.component();
        match self {
            Self::Feed | Self::Paused | Self::Jammed | Self::Feeding => {
                let object = match self {
                    Self::Feed => "feed",
                    Self::Paused => "paused",
                    Self::Feeding => "feeding",
                    _ => "jammed",
                };
                write!(
                    topic,
                    "homeassistant/{component}/feeder_{id}/{object}/config"
                )?;
            }
            Self::MealTime(i) => write!(
                topic,
                "homeassistant/{component}/feeder_{id}/meal_{}_time/config",
                i + 1
            )?,
            Self::MealPortions(i) => write!(
                topic,
                "homeassistant/{component}/feeder_{id}/meal_{}_portions/config",
                i + 1
            )?,
        }

        match self {
            // `payload_press` is 1 and stays 1: three portions is three
            // presses, which the feeder accumulates. Deliberately not retained
            // — a retained feed command is replayed on every reconnect, and a
            // boot loop would then empty the hopper.
            Self::Feed => write!(
                payload,
                concat!(
                    r#"{{"name":"Feed","unique_id":"feeder_{id}_feed","#,
                    r#""command_topic":"feeder/{id}/feed","payload_press":"1","#,
                    r#""availability_topic":"feeder/{id}/availability","#,
                ),
                id = id,
            )?,

            // `"retain": true` makes Home Assistant publish the command
            // retained, which is the whole persistence story for pause: it is
            // not in flash, so a unit that reboots comes back paused only
            // because the broker remembers.
            //
            // Not optimistic: the switch moves when the unit echoes `paused` in
            // its state, so a switch that springs back means the command never
            // landed.
            Self::Paused => write!(
                payload,
                concat!(
                    r#"{{"name":"Paused","unique_id":"feeder_{id}_paused","#,
                    r#""command_topic":"feeder/{id}/paused","state_topic":"feeder/{id}/state","#,
                    r#""value_template":"{{{{ 'ON' if value_json.paused else 'OFF' }}}}","#,
                    r#""retain":true,"optimistic":false,"#,
                    r#""availability_topic":"feeder/{id}/availability","#,
                ),
                id = id,
            )?,

            // The template must not be `{{ value_json.jammed }}`: a JSON `true`
            // renders as Python's `True`, which matches neither `payload_on`
            // nor `payload_off`, and the entity sticks at unknown.
            Self::Jammed => write!(
                payload,
                concat!(
                    r#"{{"name":"Jammed","unique_id":"feeder_{id}_jammed","#,
                    r#""state_topic":"feeder/{id}/state","#,
                    r#""value_template":"{{{{ 'ON' if value_json.jammed else 'OFF' }}}}","#,
                    r#""device_class":"problem","entity_category":"diagnostic","#,
                    r#""availability_topic":"feeder/{id}/availability","#,
                ),
                id = id,
            )?,

            // No `value_template`: the payload is already the event platform's
            // shape, `event_type` plus attributes. Published not retained, so
            // a reconnect never replays an old meal into the log.
            Self::Feeding => {
                write!(
                    payload,
                    concat!(
                        r#"{{"name":"Feeding","unique_id":"feeder_{id}_feeding","#,
                        r#""state_topic":"feeder/{id}/event","event_types":["#,
                    ),
                    id = id,
                )?;
                for (i, event_type) in EVENT_TYPES.iter().enumerate() {
                    let comma = if i == 0 { "" } else { "," };
                    write!(payload, r#"{comma}"{event_type}""#)?;
                }
                write!(
                    payload,
                    r#"],"availability_topic":"feeder/{id}/availability","#
                )?;
            }

            // Home Assistant sends `HH:MM:SS`; the unit drops the seconds.
            Self::MealTime(i) => write!(
                payload,
                concat!(
                    r#"{{"name":"Meal {n} time","unique_id":"feeder_{id}_meal_{n}_time","#,
                    r#""command_topic":"feeder/{id}/meal/{n}/time","#,
                    r#""state_topic":"feeder/{id}/schedule/state","#,
                    r#""value_template":"{{{{ value_json[{i}].time if value_json|length > {i} else 'None' }}}}","#,
                    r#""entity_category":"config","#,
                    r#""availability_topic":"feeder/{id}/availability","#,
                ),
                id = id,
                i = i,
                n = i + 1
            )?,

            // Zero switches the meal off without removing it, so the meals
            // after it keep their numbers. The ceiling is `MAX_CLICKS`, the
            // most one meal can turn at a portion scale of 100%.
            Self::MealPortions(i) => write!(
                payload,
                concat!(
                    r#"{{"name":"Meal {n} portions","unique_id":"feeder_{id}_meal_{n}_portions","#,
                    r#""command_topic":"feeder/{id}/meal/{n}/portions","#,
                    r#""state_topic":"feeder/{id}/schedule/state","#,
                    r#""value_template":"{{{{ value_json[{i}].portions if value_json|length > {i} else 'None' }}}}","#,
                    r#""min":0,"max":{max},"step":1,"mode":"box","#,
                    r#""entity_category":"config","#,
                    r#""availability_topic":"feeder/{id}/availability","#,
                ),
                id = id,
                i = i,
                n = i + 1,
                max = MAX_CLICKS
            )?,
        }
        close_with_device(payload, id, sw, ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::string::ToString as _;
    use std::vec::Vec;

    /// Every config, rendered for a real id and a long version string.
    fn rendered() -> Vec<(Entity, std::string::String, serde_json::Value)> {
        entities()
            .map(|entity| {
                let mut topic = String::new();
                let mut payload = String::new();
                entity
                    .render(
                        "99177c",
                        "10.20.30-rc.4",
                        Some([255, 255, 255, 255]),
                        &mut topic,
                        &mut payload,
                    )
                    .unwrap_or_else(|_| panic!("{entity:?} does not fit"));
                let json = serde_json::from_str(&payload)
                    .unwrap_or_else(|e| panic!("{entity:?} is not JSON: {e}\n{payload}"));
                (entity, topic.to_string(), json)
            })
            .collect()
    }

    #[test]
    fn every_config_fits_and_is_json() {
        assert_eq!(rendered().len(), 4 + 2 * MAX_SLOTS);
    }

    /// Home Assistant drops an event whose type the config did not announce,
    /// so the list must be exactly the one `events.rs` sends from.
    #[test]
    fn the_feeding_entity_announces_every_event_type() {
        let all = rendered();
        let (_, topic, json) = all
            .iter()
            .find(|(e, _, _)| *e == Entity::Feeding)
            .expect("announced");
        assert_eq!(topic, "homeassistant/event/feeder_99177c/feeding/config");
        assert_eq!(json["state_topic"], "feeder/99177c/event");
        let announced: Vec<_> = json["event_types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(announced, EVENT_TYPES);
    }

    #[test]
    fn every_entity_is_unique_and_on_one_device() {
        let all = rendered();
        let mut ids: Vec<_> = all
            .iter()
            .map(|(_, _, j)| j["unique_id"].to_string())
            .collect();
        let mut topics: Vec<_> = all.iter().map(|(_, t, _)| t.clone()).collect();
        ids.sort();
        ids.dedup();
        topics.sort();
        topics.dedup();
        assert_eq!(ids.len(), all.len());
        assert_eq!(topics.len(), all.len());

        for (entity, _, json) in &all {
            assert_eq!(
                json["device"]["identifiers"][0], "feeder_99177c",
                "{entity:?}"
            );
            assert_eq!(json["device"]["model"], "cat-feeder ESP32-C6");
            assert_eq!(json["availability_topic"], "feeder/99177c/availability");
            assert_eq!(
                json["device"]["configuration_url"],
                "http://255.255.255.255/"
            );
        }
    }

    #[test]
    fn the_meal_entities_address_their_slot_one_based_and_read_it_zero_based() {
        let all = rendered();
        let (_, topic, time) = all
            .iter()
            .find(|(e, _, _)| *e == Entity::MealTime(2))
            .unwrap();
        assert_eq!(topic, "homeassistant/time/feeder_99177c/meal_3_time/config");
        assert_eq!(time["name"], "Meal 3 time");
        assert_eq!(time["command_topic"], "feeder/99177c/meal/3/time");
        assert_eq!(time["state_topic"], "feeder/99177c/schedule/state");
        assert_eq!(
            time["value_template"],
            "{{ value_json[2].time if value_json|length > 2 else 'None' }}"
        );
        assert!(
            time.get("retain").is_none(),
            "meal commands are never retained"
        );

        let (_, topic, portions) = all
            .iter()
            .find(|(e, _, _)| *e == Entity::MealPortions(7))
            .unwrap();
        assert_eq!(
            topic,
            "homeassistant/number/feeder_99177c/meal_8_portions/config"
        );
        assert_eq!(portions["command_topic"], "feeder/99177c/meal/8/portions");
        assert_eq!(
            portions["value_template"],
            "{{ value_json[7].portions if value_json|length > 7 else 'None' }}"
        );
        assert_eq!(portions["min"], 0);
        assert_eq!(portions["max"], MAX_CLICKS);
        assert!(portions.get("retain").is_none());
    }

    /// The command topics the entities publish to are the ones the unit
    /// parses: `feeder/<id>/meal/` then what `SlotEdit::parse` reads.
    #[test]
    fn the_command_topics_parse_back_into_the_slot_they_name() {
        use crate::schedule::{SlotChange, SlotEdit};

        for (entity, _, json) in rendered() {
            let (index, payload, change) = match entity {
                Entity::MealTime(i) => (i, "07:30:00", SlotChange::Time(450)),
                Entity::MealPortions(i) => (i, "2", SlotChange::Portions(2)),
                _ => continue,
            };
            let topic = json["command_topic"].as_str().unwrap();
            let path = topic.strip_prefix("feeder/99177c/meal/").unwrap();
            assert_eq!(
                SlotEdit::parse(path, payload.as_bytes()),
                Ok(SlotEdit { index, change }),
                "{topic}"
            );
        }
    }

    /// No address, no link — never one pointing at nowhere.
    #[test]
    fn the_link_is_left_out_until_there_is_an_address() {
        let mut topic = String::new();
        let mut payload = String::new();
        Entity::Feed
            .render("99177c", "0.1.0", None, &mut topic, &mut payload)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert!(json["device"].get("configuration_url").is_none());
        assert_eq!(json["device"]["name"], "Cat feeder 99177c");
    }

    #[test]
    fn the_longest_topic_fits() {
        let longest = rendered().iter().map(|(_, t, _)| t.len()).max().unwrap();
        assert_eq!(longest, 57);
        assert!(longest <= TOPIC_LEN);
    }
}
