# Discovery payloads

- [The shared device block](#the-shared-device-block)
- [Button: feed](#button-feed)
- [Switch: paused](#switch-paused)
- [Binary sensor: jammed](#binary-sensor-jammed)
- [Event: feeding](#event-feeding)
- [State payload](#state-payload)
- [Home Assistant side](#home-assistant-side)
- [Testing without hardware](#testing-without-hardware)
- [Field reference](#field-reference)
- [Sources](#sources)

Replace `<id>` throughout with the MAC-derived device id.

## The shared device block

Identical in every payload. `identifiers` is what joins them.

```json
"device": {
  "identifiers": ["feeder_<id>"],
  "name": "Cat feeder <id>",
  "manufacturer": "DIY",
  "model": "cat-feeder ESP32-C6",
  "sw_version": "0.1.0",
  "configuration_url": "http://<address>/"
}
```

`configuration_url` is Home Assistant's *Visit device* link to the admin page,
from the address at connect time, and is left out while there is none. The
full examples below omit it for brevity; `src/discovery.rs` is the source.

`manufacturer`, `model` and `sw_version` are cosmetic. `identifiers` and `name`
are not.

## Button: feed

Topic: `homeassistant/button/feeder_<id>/feed/config`, retained.

```json
{
  "name": "Feed",
  "unique_id": "feeder_<id>_feed",
  "command_topic": "feeder/<id>/feed",
  "payload_press": "1",
  "availability_topic": "feeder/<id>/availability",
  "payload_available": "online",
  "payload_not_available": "offline",
  "device": {
    "identifiers": ["feeder_<id>"],
    "name": "Cat feeder <id>",
    "manufacturer": "DIY",
    "model": "cat-feeder ESP32-C6",
    "sw_version": "0.1.0"
  }
}
```

`payload_available` and `payload_not_available` already default to `online` and
`offline`, so they can be dropped to save bytes. They are spelled out here
because the contract fixes those words.

`payload_press` is `1` and stays `1`. Three portions means three presses, which
the firmware accumulates into a pending-portions counter. There is no `retain`
key here on purpose: a retained feed command would be replayed on every
reconnect, and with accumulation a boot loop would empty the hopper.

Home Assistant publishes one message per press with no client-side coalescing,
so rapid presses do arrive as separate commands. They may arrive while the motor
is still running, which is exactly the case the counter exists for.

## Switch: paused

Topic: `homeassistant/switch/feeder_<id>/paused/config`, retained.

```json
{
  "name": "Paused",
  "unique_id": "feeder_<id>_paused",
  "command_topic": "feeder/<id>/paused",
  "state_topic": "feeder/<id>/state",
  "value_template": "{{ 'ON' if value_json.paused else 'OFF' }}",
  "retain": true,
  "optimistic": false,
  "availability_topic": "feeder/<id>/availability",
  "device": {
    "identifiers": ["feeder_<id>"],
    "name": "Cat feeder <id>",
    "manufacturer": "DIY",
    "model": "cat-feeder ESP32-C6",
    "sw_version": "0.1.0"
  }
}
```

`"retain": true` makes Home Assistant publish the command with the retain flag,
so a unit that reboots comes back paused. The paused flag is not stored in
flash — unlike the schedule — so the broker is the only thing that remembers
it.

Unlike the number entity this replaces, the switch is **not** optimistic. The
device echoes `paused` in its state payload, so the switch flips only once the
unit has actually acknowledged it. If the switch springs back after a moment,
the unit never applied the command.

`ON` and `OFF` are the defaults for `payload_on` and `payload_off`, which is why
the command topic carries those literal words and neither key is set here.

No `entity_category`. Pausing is a primary control, not configuration, so it
belongs in the main controls of the device page rather than tucked into the
config section.

## Binary sensor: jammed

Topic: `homeassistant/binary_sensor/feeder_<id>/jammed/config`, retained.

```json
{
  "name": "Jammed",
  "unique_id": "feeder_<id>_jammed",
  "state_topic": "feeder/<id>/state",
  "value_template": "{{ 'ON' if value_json.jammed else 'OFF' }}",
  "device_class": "problem",
  "entity_category": "diagnostic",
  "availability_topic": "feeder/<id>/availability",
  "device": {
    "identifiers": ["feeder_<id>"],
    "name": "Cat feeder <id>",
    "manufacturer": "DIY",
    "model": "cat-feeder ESP32-C6",
    "sw_version": "0.1.0"
  }
}
```

`device_class: problem` makes Home Assistant render `ON` as "Problem" in red,
which is the right polarity for a jam.

Do not write `"value_template": "{{ value_json.jammed }}"`. A JSON `true`
reaches the template engine as a Python `True` and renders as the string
`True`, which equals neither the default `payload_on` (`ON`) nor `payload_off`
(`OFF`), so the entity sticks at unknown.

## Event: feeding

Topic: `homeassistant/event/feeder_<id>/feeding/config`, retained. Rendered
from `events::EVENT_TYPES`, so the list cannot drift from what is sent.

```json
{
  "name": "Feeding",
  "unique_id": "feeder_<id>_feeding",
  "state_topic": "feeder/<id>/event",
  "event_types": ["scheduled", "skipped", "manual", "jammed"],
  "availability_topic": "feeder/<id>/availability",
  "device": { "...": "the shared block" }
}
```

No `value_template`: the payload on `feeder/<id>/event` is already the event
platform's shape, an `event_type` plus attributes. Published **not retained**,
so a reconnect never replays an old meal into the log. Home Assistant drops
an event whose `event_type` is not in the config's list.

```json
{"event_type":"scheduled","portions":2,"slot":"08:00","at":"2026-10-01T08:00:00+02:00"}
{"event_type":"skipped","slot":"19:00","reason":"paused","at":"..."}
{"event_type":"skipped","slot":"19:00","reason":"late","at":"..."}
{"event_type":"manual","portions":1,"source":"knob","at":"..."}
{"event_type":"manual","portions":3,"source":"web","at":"..."}
{"event_type":"jammed","at":"..."}
```

`at` is the unit's trusted time when it happened, and is left out when it has
none. An event raised while the broker is down waits in a queue of eight and
arrives late, stamped by Home Assistant with its arrival; `at` is the real
time. A feed sent *from* Home Assistant raises no event — Activity already
has the button press. `src/events.rs` is the source of truth.

## Meal n: time and portions

Sixteen entities, `n` = 1..8, rendered by `src/discovery.rs` — copy from there
if this drifts. Meal 3 shown; the index in the template is `n - 1`.

Topic: `homeassistant/time/feeder_<id>/meal_3_time/config`, retained.

```json
{
  "name": "Meal 3 time",
  "unique_id": "feeder_<id>_meal_3_time",
  "command_topic": "feeder/<id>/meal/3/time",
  "state_topic": "feeder/<id>/schedule/state",
  "value_template": "{{ value_json[2].time if value_json|length > 2 else 'None' }}",
  "entity_category": "config",
  "availability_topic": "feeder/<id>/availability",
  "device": { "...": "the shared block" }
}
```

Topic: `homeassistant/number/feeder_<id>/meal_3_portions/config`, retained.

```json
{
  "name": "Meal 3 portions",
  "unique_id": "feeder_<id>_meal_3_portions",
  "command_topic": "feeder/<id>/meal/3/portions",
  "state_topic": "feeder/<id>/schedule/state",
  "value_template": "{{ value_json[2].portions if value_json|length > 2 else 'None' }}",
  "min": 0, "max": 16, "step": 1, "mode": "box",
  "entity_category": "config",
  "availability_topic": "feeder/<id>/availability",
  "device": { "...": "the shared block" }
}
```

What Home Assistant sends, read from its `mqtt/time.py` and `mqtt/number.py`:
`value.isoformat()`, so `08:00:00`, and an integer as a bare `2`. The unit
drops the seconds. `None` from a template is the platforms' "unknown", which
is what a position past the end of the schedule shows.

The unit's side of each rule is a host test in `schedule.rs`: zero switches a
meal off in place; a time past the end adds the meal switched off; portions
for a meal with no time are refused. Two more live in gated code and were seen
on hardware rather than tested: `main.rs` republishes the unchanged echo so
the entity springs back, and `mqtt.rs` refuses a retained edit.

To drive one without Home Assistant:

```sh
docker compose exec mosquitto mosquitto_pub -u <user> -P <pass> \
  -t 'feeder/<id>/meal/3/time' -m '12:30:00'     # never -r
```

## State payload

Published retained to `feeder/<id>/state` on every transition:

```json
{"feeding": false, "jammed": false, "paused": false, "meals": 2, "last_fed": "2026-09-14T08:00:00+02:00"}
```

`meals` is how many slots the unit holds **with at least one portion**; a slot
switched off with `0` is not counted. `0` is a unit that is online and healthy
and will never feed — a new or factory-reset one that has not been sent a
schedule, or one with every meal switched off — so it is worth watching for.

`paused` must be published from the flag the device is actually acting on, not
echoed back from the command as it arrives. Echoing makes the switch look
correct in Home Assistant even when the schedule task never saw the change.

`feeding` is deliberately not exposed as an entity. The entity list is
`discovery::entities()`, and adding one is a change to make on purpose, not by
drift.

The pending-portions counter is not published either. It is in RAM, it drains
within seconds, and a Home Assistant entity that lags a few seconds behind a
counter is worse than no entity.

## Home Assistant side

Home Assistant publishes the time, retained, to a topic with no `<id>`, so all
three feeders read the same thing. Each unit also keeps its own time in a
DS3231, and a set one arms the schedule at boot without Home Assistant; the
live time is what keeps it corrected, and what arms a unit whose RTC was never
set. The schedule belongs to the units, in flash — Home Assistant only sends
it.

```yaml
# publish every minute, on restart, and whenever a feeder asks
triggers:
  - trigger: time_pattern
    minutes: "/1"
  - trigger: homeassistant
    event: start
  - trigger: mqtt
    topic: feeder/time/request
actions:
  - action: mqtt.publish
    data:
      topic: feeder/time
      retain: true
      payload: "{{ now().isoformat() }}"
```

**The `feeder/time/request` trigger belongs on this automation, not a second
one.** A feeder whose RTC is not set arms its schedule only on a *live* time,
and every feeder asks for one as the last step of connecting; without this trigger it waits for the next minute
boundary instead, which is up to a minute of every boot and every reconnect
spent not feeding. Two automations publishing the same topic is how they drift,
so it goes here. `mode: single` is fine — three feeders rebooting together send
three requests within milliseconds, two are dropped, and the one publish that
happens is forwarded live to all three.

**`now()`, never `utcnow()`.** The firmware reads the wall-clock fields and does
not apply the offset: schedule slots are local times and the feeders share a
house with the broker, so `08:00` already means 08:00 on the wall. Publishing
UTC would still look like a valid time while moving every meal by the offset,
with nothing failing. The offset is parsed and logged at startup so that
mistake is visible on the console, but nothing rejects it.

Not applying the offset is also what makes daylight saving free — in October
`now()` simply starts rendering `+01:00` and the wall-clock fields shift with
it. The unit's own timezone (`tz.rs`) only takes over after ten minutes
without a live time.

`now().isoformat()` renders a bare ISO 8601 string with microseconds,
`2026-09-14T08:00:00.123456+02:00`, not a quoted JSON string. The firmware
accepts that, a quoted string, a trailing `Z`, `+HHMM`, and no offset at all.

`script.cat_feeder_copy_schedule` is the Home Assistant side: it reads one
feeder's `Meal n` entities and publishes them, per target, as:

```yaml
- action: mqtt.publish
  data:
    topic: feeder/<id>/schedule        # one publish per target unit
    retain: false
    payload: '[{"time":"08:00","portions":2},{"time":"19:00","portions":2}]'
```

The `Meal n time` state is `HH:MM:SS` and the firmware takes exactly `HH:MM`,
so the script cuts it to five characters. There is no `feeder/all/schedule`:
it was removed on 2026-10-01 (CLAUDE.md, v2 point 6), and syncing is this
per-unit copy.

**Never retained.** A retained schedule command would hand meals to every unit
that subscribes later; the firmware refuses one replayed at subscribe time
(`mqtt: ignored a retained schedule command; publish it without retain`). A unit
already subscribed *does* act on a retained publish, because the broker forwards
it live with the retain flag cleared — so the rule is never to publish one
retained, not to rely on the refusal. `feeder/<id>/schedule` does the same
for one unit. Each unit logs `schedule: N meals, stored` (or `unchanged`) and
echoes what it holds on `feeder/<id>/schedule/state`, retained, which is where to
check the result.

Feeding all three at once is a single publish to `feeder/all/feed`.

Pausing all three takes one publish per unit, because the paused flag is
retained per unit and has no broadcast topic:

```yaml
# pause every feeder
- repeat:
    for_each: ["<id1>", "<id2>", "<id3>"]
    sequence:
      - service: mqtt.publish
        data:
          topic: "feeder/{{ repeat.item }}/paused"
          retain: true
          payload: "ON"
```

For a weekend-only house, drive that from a schedule helper rather than two
time triggers, so a bank holiday is one drag in the user interface instead of an
automation edit:

```yaml
- triggers:
    - trigger: state
      entity_id: schedule.cats_at_home
  variables:
    # Found, not listed: discovery gave each unit a device with this `model`
    # and `identifiers` of ["mqtt", "feeder_<id>"], so a feeder added to the
    # broker joins by itself and no device id is written down.
    units: >-
      {%- set ns = namespace(ids=[]) -%}
      {%- for e in integration_entities('mqtt')
                   | select('is_device_attr', 'model', 'cat-feeder ESP32-C6') -%}
        {%- set ns.ids = ns.ids +
            [(device_attr(e, 'identifiers') | list | first)[1]
             | replace('feeder_', '')] -%}
      {%- endfor -%}
      {{ ns.ids | unique | list }}
  actions:
    - repeat:
        for_each: "{{ units }}"
        sequence:
          - action: mqtt.publish
            data:
              topic: "feeder/{{ repeat.item }}/paused"
              retain: true
              payload: "{{ 'OFF' if trigger.to_state.state == 'on' else 'ON' }}"
```

⚠️ **Publish to the topic; do not call `switch.turn_on` on the discovered
switch.** It looks equivalent and needs no id at all, but Home Assistant drops
unavailable entities from an entity service call, and every feeder's switch
carries an `availability_topic`. Pausing while a unit is offline would then do
nothing — and an offline unit is exactly the one whose pause has to be waiting
in the broker when it comes back.

A reminder that fires if a feeder has been paused for more than a few days is
an obvious addition, because a forgotten pause is silent by design and is the
only state in this system where nothing alarms and the cats do not eat. It is
**not** in the package, and that is a decision about how these feeders are
used rather than an omission: the schedule here is paused when somebody is home
to feed by hand, so the reminder would fire on the normal case. Add it for a
feeder that normally runs unattended.

## Testing without hardware

Against the dev broker in Docker on the Mac:

```sh
# watch everything the firmware does
mosquitto_sub -h localhost -p 1883 -u <user> -P <pass> -v -t 'feeder/#' -t 'homeassistant/#'

# manual feed
mosquitto_pub -h localhost -p 1883 -u <user> -P <pass> -t 'feeder/<id>/feed' -m '2'

# three rapid presses: the device must produce 3 portions, not 1
for i in 1 2 3; do
  mosquitto_pub -h localhost -p 1883 -u <user> -P <pass> -t 'feeder/<id>/feed' -m '1'
done

# pause, then confirm a manual feed still works
mosquitto_pub -h localhost -p 1883 -u <user> -P <pass> -r -t 'feeder/<id>/paused' -m 'ON'
mosquitto_pub -h localhost -p 1883 -u <user> -P <pass> -t 'feeder/<id>/feed' -m '1'

# pretend to be Home Assistant
mosquitto_pub -h localhost -p 1883 -u <user> -P <pass> -r -t 'feeder/time' \
  -m '"2026-09-14T08:00:00+02:00"'

# clear a stale retained discovery config
mosquitto_pub -h localhost -p 1883 -u <user> -P <pass> -r -n \
  -t 'homeassistant/button/feeder_<id>/feed/config'
```

Subscribing to `homeassistant/#` and seeing no config message means the
firmware never published it, or the payload was truncated by an undersized
buffer.

## Field reference

Discovery topic shape, from the Home Assistant MQTT integration:

```
<discovery_prefix>/<component>/[<node_id>/]<object_id>/config
```

`discovery_prefix` defaults to `homeassistant`. This project uses
`feeder_<id>` as `node_id` and the entity name as `object_id`. Both segments
accept only `[a-zA-Z0-9_-]`.

Device block keys: `identifiers`, `name`, `manufacturer`, `model`,
`sw_version`, `hw_version`, `serial_number`, `model_id`, `configuration_url`.

Availability keys: `availability_topic`, `payload_available` (default
`online`), `payload_not_available` (default `offline`), `availability_mode`
(`all`, `any`, `latest`), `availability_template`.

Home Assistant also accepts abbreviations such as `dev` for `device`,
`uniq_id` for `unique_id` and `cmd_t` for `command_topic`. This project does not
use them.

## Sources

- MQTT integration and discovery: <https://www.home-assistant.io/integrations/mqtt/>
- MQTT button: <https://www.home-assistant.io/integrations/button.mqtt/>
- MQTT number: <https://www.home-assistant.io/integrations/number.mqtt/>
- MQTT binary sensor: <https://www.home-assistant.io/integrations/binary_sensor.mqtt/>
