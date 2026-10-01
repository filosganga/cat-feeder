---
name: ha-mqtt-discovery
description: Supplies this project's MQTT topic contract and the exact Home Assistant discovery payloads for the feed button, the paused switch, the jammed binary sensor, the Feeding event and the Meal n time/portions entities, so they are copied rather than reconstructed. Use when writing or reviewing mqtt.rs, changing a topic or payload, adding an entity, debugging an entity that does not appear in Home Assistant or shows as unavailable, or writing the Home Assistant side that publishes the time and sends the schedule.
---

# Home Assistant MQTT discovery for cat-feeder

Broker: Mosquitto on port 1883 with username and password. The dev broker runs
in Docker from this repo's `compose.yaml`; a deployed one is wherever that
install's Home Assistant lives, and nothing here needs to know.

`<id>` is the device id derived from the MAC. Keep the derivation in one
function and reuse it for every topic; the discovery `node_id` and `object_id`
only accept characters from `[a-zA-Z0-9_-]`, so lowercase hex is safe.

## Topic contract

| Topic | Payload | Direction | Retained |
|---|---|---|---|
| `feeder/<id>/availability` | `online` / `offline` | device → | yes, and the will |
| `feeder/<id>/feed` | `<portions:u8>` | → device | **no** |
| `feeder/all/feed` | `<portions:u8>` | → all devices | **no** |
| `feeder/<id>/paused` | `ON` / `OFF` | → device, and device → from the knob's menu | yes |
| `feeder/<id>/schedule` | `[{"time":"08:00","portions":2}]` | → one device | **no** |
| `feeder/<id>/schedule/state` | `[{"time":"08:00","portions":2}]`, `[]` for none | device → | yes |
| `feeder/<id>/meal/<n>/time` | `08:00:00` (or `08:00`), n = 1..8 | → device, from *Meal n time* | **no** |
| `feeder/<id>/meal/<n>/portions` | `2`; `0` switches the meal off | → device, from *Meal n portions* | **no** |
| `feeder/time` | `"2026-09-14T08:00:00+02:00"` | HA → all, each minute | yes |
| `feeder/time/request` | `<id>` | device → HA | **no** |
| `feeder/<id>/state` | `{"feeding":bool,"jammed":bool,"paused":bool,"meals":n,"last_fed":"..."}` | device → | yes |
| `feeder/<id>/event` | `{"event_type":"scheduled","portions":2,"slot":"08:00","at":"..."}` | device → | **no** |

The unit owns its schedule and keeps it in flash (its own sector, `FDS1`), and
keeps its time in a DS3231. What stays broker state is the time — retained so
a unit has *a* time to log, though only a live one or a set RTC arms the
schedule — and the paused flag, which is not in flash and is replayed on every
reconnect. `feeder/<id>/schedule/state` is retained too, but only as an echo of what
the unit holds, published on every connect and every change; the unit never
reads it back.

The two `feed` topics, the two schedule commands and the meal edits must **never** be
retained. A retained feed command is replayed on every reconnect, and because
manual feeds accumulate, a boot loop would empty the hopper. A retained
schedule command would hand meals to every unit that subscribes later, which
is exactly what *a new unit starts blank* exists to prevent: the firmware
refuses one replayed at subscribe time (`mqtt: ignored a retained schedule
command; publish it without retain`). It cannot refuse a retained publish made
while it is already subscribed — the broker forwards that live with the retain
flag cleared — so the rule is on the publisher.

## Manual feeds accumulate

The button always sends `1`. Pressing it three times means three portions, even
when the presses land while the motor is still running. So `feed` adds to a
pending counter that the `feeder` task drains; it never replaces the count and
never drops a command because the feeder is busy.

**Every topic here speaks portions; the feeder counts clicks.** The three units
are not all the same model, so each carries a portion scale in its record and
`Feeder::request` converts. A slot saying `portions: 2` therefore reaches all
three identically and each turns as far as its own mechanism needs. Nothing on
the MQTT side has to know about this — but it is why `portions` in a payload is
not necessarily the number of clicks a given unit will make.

Cap the counter at `MAX_CLICKS` (16) and log a warning when clamping. The cap
counts clicks rather than portions, because clicks are what empty a hopper.
Two things make it matter: a stuck Home Assistant automation, and MQTT QoS 1,
which is allowed to deliver the same publish twice.

There is no default portion size. Every feed path states its own count — the
button as `1`, each schedule slot as its own `portions`.

## Pause stops the schedule, not the feeder

`feeder/<id>/paused` is retained and per unit. While it is `ON`:

- Scheduled slots do **not** feed. They are marked consumed anyway, so resuming
  never replays a slot and never catches one up.
- Manual `feed` still works. Pause is for the schedule only, so a bowl can
  always be topped up by hand.
- The unit stays `online` and keeps re-aligning its clock. Paused is not
  offline, and the feed button must not grey out.

There is deliberately **no `feeder/all/paused`**. Two retained topics setting
the same flag would race on reconnect, with no defined winner. Home Assistant
pauses all three by publishing to each unit's own topic, in one automation that
finds the units in the device registry by the `model` in the discovery payload
rather than being given a list. That makes `model` a contract: change it here
and pause quietly stops matching anything. See `references/payloads.md`, which
also says why it must publish rather than call `switch.turn_on`.

A feeder left paused is the one state where cats do not eat and nothing alarms,
which is why `paused` appears both in the state payload and as a switch.

## Connection order

Get this wrong and entities appear unavailable or never appear at all.

1. In the MQTT CONNECT packet, set the last will: topic
   `feeder/<id>/availability`, payload `offline`, **retain true**, QoS 1.
2. After CONNACK, publish every discovery config `discovery::entities()` names
   — nineteen today — each **retained**, to
   `homeassistant/<component>/feeder_<id>/<object>/config`.
3. Only then publish `online` to `feeder/<id>/availability`, retained.
3a. If the knob's menu changed the pause while the broker was unreachable,
   publish it now to `feeder/<id>/paused`, retained — **before** step 4, so the
   retained replay the subscription triggers carries the new value back rather
   than undoing it.
4. Subscribe to `feeder/<id>/feed`, `feeder/all/feed`, `feeder/<id>/paused`,
   `feeder/<id>/schedule`, `feeder/<id>/meal/+/+`,
   `feeder/time`.
4a. Publish what the unit holds to `feeder/<id>/schedule/state`, retained — `[]` for
   none — because the broker's copy may be from before a reboot or a factory
   reset.
5. Publish this unit's id to `feeder/time/request`, so Home Assistant sends a
   live time now instead of at the next minute boundary. See
   `docs/adr/0003-a-retained-time-is-not-a-trusted-time.md`.
6. Publish the first `feeder/<id>/state`, retained.

**Step 5 must come after step 4**, and that ordering is the whole trick: a reply
that arrives before the subscription exists is a reply nobody hears. It is also
why the request is sent once per connection rather than on a timer — it exists
to collapse the initial wait, not to poll. Measured on a Zero: the schedule arms
626 ms after the request instead of up to a minute later.

Step 4 is where the retained paused flag arrives. Do not publish a state payload
claiming `"paused": false` before that subscription has had a chance to deliver
it, or Home Assistant will briefly show a paused feeder as running.

Because the discovery messages are retained, Home Assistant re-reads them after
a restart on its own. The firmware does not need to subscribe to
`homeassistant/status`.

## The entities

Full JSON, ready to copy, is in [references/payloads.md](references/payloads.md).
Topics used:

| Component | Discovery topic | Purpose |
|---|---|---|
| `button` | `homeassistant/button/feeder_<id>/feed/config` | feed one portion |
| `switch` | `homeassistant/switch/feeder_<id>/paused/config` | pause the schedule |
| `binary_sensor` | `homeassistant/binary_sensor/feeder_<id>/jammed/config` | jam alarm |
| `event` | `homeassistant/event/feeder_<id>/feeding/config` | meals served or skipped, feeds at the unit, jams — Activity |
| `time` ×8 | `homeassistant/time/feeder_<id>/meal_<n>_time/config` | meal *n*'s time |
| `number` ×8 | `homeassistant/number/feeder_<id>/meal_<n>_portions/config` | meal *n*'s portions, 0–16 |

**The payloads are rendered by `src/discovery.rs`, which is the source of
truth**; its host tests parse every one as JSON. The meal entities read their
slot out of `feeder/<id>/schedule/state` by position —
`{{ value_json[2].time if value_json|length > 2 else 'None' }}` for meal 3 — and
the literal `None` is what both platforms show as unknown. Neither is
optimistic, and neither sets `retain`, so Home Assistant's default of not
retaining applies.

All of them carry the same `device` block and a `unique_id`, which is what makes
Home Assistant group them into one device. Without `unique_id` the `device`
block is ignored and the entities appear loose.

## Rules

- **Never abbreviate.** Home Assistant accepts `cmd_t` for `command_topic`, but
  the full names keep the firmware greppable. Pick one style and it is this one.
- **Never template a JSON boolean directly.** `{{ value_json.jammed }}` renders
  Python's `True`/`False`, which matches neither `payload_on` nor `payload_off`.
  Use `{{ 'ON' if value_json.jammed else 'OFF' }}`.
- **Size the publish buffer for the payloads**, roughly 400 bytes each with the
  device block. A silently truncated discovery message is a common cause of a
  missing entity.
- **Clearing an entity** means publishing an empty retained payload to its
  discovery topic. Leaving a stale retained config behind is why a renamed
  entity shows up twice.
- `feeder/all/feed` gets no discovery entity. It is a broadcast that Home
  Assistant automations publish directly, and it is how three feeders feed at
  the same instant.
- **A payload of `0` is a no-op**, not an error. Log it and ignore it.

## Feeding more than one portion from Home Assistant

The button is fixed at one portion, so the user interface for "three portions"
is pressing it three times. Anything that needs a specific count publishes the
number directly, which any automation or script can do:

```yaml
- service: mqtt.publish
  data:
    topic: feeder/<id>/feed
    payload: "3"
```

Resist re-adding a `number` entity for a default portion size. It had no
consumer: `feed` and every schedule slot already carry their own count, so the
only thing that ever read it was the button, which now sends `1`.
