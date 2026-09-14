# Discovery payloads

- [The shared device block](#the-shared-device-block)
- [Button: feed](#button-feed)
- [Switch: paused](#switch-paused)
- [Binary sensor: jammed](#binary-sensor-jammed)
- [State payload](#state-payload)
- [Home Assistant side](#home-assistant-side)
- [Testing without hardware](#testing-without-hardware)
- [Field reference](#field-reference)
- [Sources](#sources)

Replace `<id>` throughout with the MAC-derived device id.

## The shared device block

Identical in all three payloads. `identifiers` is what joins them.

```json
"device": {
  "identifiers": ["feeder_<id>"],
  "name": "Cat feeder <id>",
  "manufacturer": "DIY",
  "model": "cat-feeder ESP32-C6",
  "sw_version": "0.1.0"
}
```

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
so a unit that reboots comes back paused. Nothing is stored in flash, so the
broker is the only thing that remembers.

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

## State payload

Published retained to `feeder/<id>/state` on every transition:

```json
{"feeding": false, "jammed": false, "paused": false, "last_fed": "2026-09-14T08:00:00+02:00"}
```

`paused` must be published from the flag the device is actually acting on, not
echoed back from the command as it arrives. Echoing makes the switch look
correct in Home Assistant even when the schedule task never saw the change.

`feeding` is deliberately not exposed as an entity. The contract lists three
entities and adding a fourth is a change to make on purpose, not by drift.

The pending-portions counter is not published either. It is in RAM, it drains
within seconds, and a Home Assistant entity that lags a few seconds behind a
counter is worse than no entity.

## Home Assistant side

Home Assistant owns the clock and the schedule. Both are retained, published to
topics with no `<id>`, so all three feeders read the same thing.

```yaml
# publish every minute
- service: mqtt.publish
  data:
    topic: feeder/time
    retain: true
    payload: "{{ now().isoformat() }}"
```

```yaml
# publish once, and whenever the schedule changes
- service: mqtt.publish
  data:
    topic: feeder/schedule
    retain: true
    payload: '[{"time":"08:00","portions":2},{"time":"19:00","portions":2}]'
```

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
- trigger:
    - platform: state
      entity_id: schedule.cats_at_home
  action:
    - repeat:
        for_each: ["<id1>", "<id2>", "<id3>"]
        sequence:
          - service: mqtt.publish
            data:
              topic: "feeder/{{ repeat.item }}/paused"
              retain: true
              payload: "{{ 'OFF' if trigger.to_state.state == 'on' else 'ON' }}"
```

Add a reminder that fires if every feeder has been paused for more than a few
days. A forgotten pause is silent by design, and it is the only state in this
system where nothing alarms and the cats do not eat.

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
`sw_version`, `hw_version`, `serial_number`, `model_id`.

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
