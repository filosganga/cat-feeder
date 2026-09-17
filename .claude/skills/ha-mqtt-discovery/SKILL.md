---
name: ha-mqtt-discovery
description: Supplies this project's MQTT topic contract and the exact Home Assistant discovery payloads for the feed button, the paused switch and the jammed binary sensor, so they are copied rather than reconstructed. Use when writing or reviewing mqtt.rs, changing a topic or payload, adding an entity, debugging an entity that does not appear in Home Assistant or shows as unavailable, or writing the Home Assistant side that publishes time and schedule.
---

# Home Assistant MQTT discovery for cat-feeder

Broker: Mosquitto on port 1883 with username and password. Dev broker runs in
Docker on the Mac, production on the Raspberry Pi 5.

`<id>` is the device id derived from the MAC. Keep the derivation in one
function and reuse it for every topic; the discovery `node_id` and `object_id`
only accept characters from `[a-zA-Z0-9_-]`, so lowercase hex is safe.

## Topic contract

| Topic | Payload | Direction | Retained |
|---|---|---|---|
| `feeder/<id>/availability` | `online` / `offline` | device → | yes, and the will |
| `feeder/<id>/feed` | `<portions:u8>` | → device | **no** |
| `feeder/all/feed` | `<portions:u8>` | → all devices | **no** |
| `feeder/<id>/paused` | `ON` / `OFF` | → device | yes |
| `feeder/schedule` | `[{"time":"08:00","portions":2}]` | HA → all | yes |
| `feeder/time` | `"2026-09-14T08:00:00+02:00"` | HA → all, each minute | yes |
| `feeder/time/request` | `<id>` | device → HA | **no** ⬜ |
| `feeder/<id>/state` | `{"feeding":bool,"jammed":bool,"paused":bool,"last_fed":"..."}` | device → | yes |

The schedule, the time and the paused flag are retained because the firmware
keeps nothing in flash. On reboot the device re-subscribes and the broker
replays all three. That is the whole persistence story — do not add flash
storage to work around a missing retain flag.

The two `feed` topics are the exception and must **never** be retained. A
retained feed command is replayed on every reconnect, and because manual feeds
accumulate, a boot loop would empty the hopper.

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
pauses all three by publishing to each unit's own topic, which one automation
does in three lines.

A feeder left paused is the one state where cats do not eat and nothing alarms,
which is why `paused` appears both in the state payload and as a switch.

## Connection order

Get this wrong and entities appear unavailable or never appear at all.

1. In the MQTT CONNECT packet, set the last will: topic
   `feeder/<id>/availability`, payload `offline`, **retain true**, QoS 1.
2. After CONNACK, publish the three discovery configs, each **retained**, to
   `homeassistant/<component>/feeder_<id>/<object>/config`.
3. Only then publish `online` to `feeder/<id>/availability`, retained.
4. Subscribe to `feeder/<id>/feed`, `feeder/all/feed`, `feeder/<id>/paused`,
   `feeder/schedule`, `feeder/time`.
5. Publish the first `feeder/<id>/state`, retained.
6. ⬜ **Not built:** publish this unit's id to `feeder/time/request`, so Home
   Assistant sends a live time now instead of at the next minute boundary. See
   *Asking for the time instead of waiting for it* in CLAUDE.md.

**Step 6 must come after step 4**, and that ordering is the whole trick: a reply
that arrives before the subscription exists is a reply nobody hears. It is also
why the request is sent once per connection rather than on a timer — it exists
to collapse the initial wait, not to poll.

Step 4 is where the retained paused flag arrives. Do not publish a state payload
claiming `"paused": false` before that subscription has had a chance to deliver
it, or Home Assistant will briefly show a paused feeder as running.

Because the discovery messages are retained, Home Assistant re-reads them after
a restart on its own. The firmware does not need to subscribe to
`homeassistant/status`.

## The three entities

Full JSON, ready to copy, is in [references/payloads.md](references/payloads.md).
Topics used:

| Component | Discovery topic | Purpose |
|---|---|---|
| `button` | `homeassistant/button/feeder_<id>/feed/config` | feed one portion |
| `switch` | `homeassistant/switch/feeder_<id>/paused/config` | pause the schedule |
| `binary_sensor` | `homeassistant/binary_sensor/feeder_<id>/jammed/config` | jam alarm |

All three carry the same `device` block and a `unique_id`, which is what makes
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
