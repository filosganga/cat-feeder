# Local development stack

A Mosquitto broker and a Home Assistant instance running on this Mac, so
firmware work needs no Raspberry Pi. The broker is configured to behave like
the production one: same port, same authentication, same retained-message
persistence.

## Start it

```sh
./dev/bootstrap.sh          # once, creates the broker password file
docker compose up -d
```

`bootstrap.sh` defaults to user `feeder` and password `feeder-dev`. Pass your
own as two arguments if you prefer. The password file is git-ignored.

Broker only, skipping Home Assistant:

```sh
docker compose up -d mosquitto
```

## The broker has three different addresses

This is the thing that wastes an afternoon. Same broker, three names, depending
on who is asking.

| Asking from | Host to use |
|---|---|
| This Mac, shell or GUI client | `localhost` |
| The Home Assistant container | `mosquitto` |
| The ESP32, over Wi-Fi | this Mac's LAN address |

Read the LAN address with:

```sh
ipconfig getifaddr en0
```

It comes from DHCP and will change eventually. When the firmware suddenly
cannot connect and nothing else changed, check this first. Giving the Mac a
DHCP reservation on the router avoids the whole problem.

## Connect Home Assistant to the broker

Open <http://localhost:8123> and complete the one-time onboarding to create a
local account. Then:

1. Settings, then Devices & Services, then Add Integration.
2. Choose MQTT.
3. Broker `mosquitto`, port `1883`, username and password from bootstrap.

Use `mosquitto`, not `localhost`. Inside that container `localhost` is Home
Assistant itself.

Discovery needs no configuration. It is on by default with the prefix
`homeassistant`, which is what the firmware publishes to, so feeders appear as
devices on their own once they publish their config.

## Home Assistant automations

Discovery gives you the three entities. It does **not** give you the schedule:
the feeders have no clock of their own, so until something publishes
`feeder/time` they wait forever and never feed. That half lives in
[`homeassistant/packages/cat_feeder.yaml`](../homeassistant/packages/cat_feeder.yaml),
which is tracked in this repo and used unchanged on the Raspberry Pi.

It is a Home Assistant *package*, so one file carries the automations, the
schedule helper and the feed-all script together. Install it by copying it in
and enabling packages:

```sh
mkdir -p dev/homeassistant/packages
cp homeassistant/packages/cat_feeder.yaml dev/homeassistant/packages/
```

```yaml
# dev/homeassistant/configuration.yaml, once
homeassistant:
  packages: !include_dir_named packages
```

Then restart Home Assistant — `docker compose restart homeassistant` — and
confirm the broker starts filling up, which takes at most a minute:

```sh
./dev/watch.sh 'feeder/time' 'feeder/schedule'
```

```
feeder/time 2026-09-15T19:51:00.489888+02:00
feeder/schedule [{"time":"08:00","portions":2},{"time":"19:00","portions":2}]
```

### The same thing on the Raspberry Pi

The Pi runs Raspberry Pi OS with Docker, so it is the same shape as the stack
above and the procedure is identical: copy the file into whatever directory is
mounted as Home Assistant's `/config`, add the same `packages:` line, restart
the container. It goes over **unchanged** — nothing in it names a host, which is
why it is tracked in this repo rather than configured per machine.

The same check works against the Pi by setting `MQTT_HOST` — and `MQTT_PASS`
too, if the feeder user there has a different password from the dev stack's:

```sh
MQTT_HOST=<pi> ./dev/watch.sh 'feeder/time' 'feeder/schedule'
```

A line a minute on `feeder/time` is what says the Pi's half is done, and no
feeder has to be involved at all.

Two things to check there that this compose file already gets right:

- **The container's timezone.** `compose.yaml` sets `TZ: Europe/Rome`, and that
  is load-bearing rather than cosmetic. Home Assistant publishes its own local
  time and the feeders apply the wall-clock fields directly, without converting
  — see *MQTT contract* in CLAUDE.md. Home Assistant left on UTC publishes
  `+00:00`, every meal silently moves by the offset, and the payload still looks
  entirely valid. A feeder prints what it was told at startup
  (`clock: started, ...+02:00`) for exactly this reason: it turns an hour-long
  error into the first line on the console.
- **That Mosquitto listens on the LAN**, not just on loopback, or the feeders
  cannot reach it even though the Pi's own `mosquitto_sub` works fine. The
  compose file publishes 1883 on all interfaces for the same reason.

⚠️ **Do not point one feeder at both stacks.** Every piece of persistent state
in this design is a retained message, so a unit moved back to the Mac picks up
whatever *that* broker last held — quite possibly a schedule from last week,
which is indistinguishable from a current one. Each unit points at one broker,
and changing it is a `./dev/provision.sh` run rather than something that can
happen by accident.

What the package sets up:

| Automation | When | Publishes |
|---|---|---|
| publish the time | every minute, and on restart | `feeder/time`, retained |
| publish the schedule | on restart, or the `cat_feeder_republish_schedule` event | `feeder/schedule`, retained |
| pause when away | `schedule.cat_feeder_active` changes | `feeder/<id>/paused` per unit, retained |
| warn when paused for days | a unit paused 48 hours | a notification, nothing on MQTT |

Plus `script.cat_feeder_feed_all`, which publishes one `feeder/all/feed` so all
three turn at the same instant rather than being staggered by three round
trips.

**`TZ: Europe/Rome` in `compose.yaml` is load-bearing.** Home Assistant owns the
clock, the firmware reads the wall-clock fields and does not apply the offset,
and the container defaults to UTC. Without that variable every meal lands an
hour or two out while everything still looks healthy. The feeder prints the
offset it received at startup — `clock: started, 2026-09-15T19:56:00+02:00` —
which is the only place the mistake shows.

To change feeding times, edit `meals` in the package, copy it in again, and
restart. Verified end to end: a slot published this way fired at exactly its
time, and the unit reported `"last_fed":"2026-09-15T19:56:00+02:00"`.

## Everyday commands

```sh
./dev/watch.sh                          # tail feeder/# and homeassistant/#
./dev/watch.sh 'feeder/+/state'         # one filter instead
MQTT_HOST=<pi> ./dev/watch.sh           # ...against the Pi's broker instead

docker compose logs -f mosquitto        # connects, disconnects, auth failures
docker compose logs -f homeassistant

docker compose down                     # stop, keep retained messages
docker compose down -v                  # stop and wipe every retained message
```

Publishing by hand, standing in for Home Assistant:

```sh
docker compose exec mosquitto mosquitto_pub -h localhost -u feeder -P feeder-dev \
  -r -t 'feeder/time' -m '"2026-09-14T08:00:00+02:00"'
```

## Wiping retained state

The firmware stores nothing in flash. The broker holds the schedule, the time,
the paused flag and every discovery config, so a stale retained message looks
exactly like a firmware bug.

```sh
docker compose down -v && docker compose up -d
```

That is also the only honest way to test a cold boot: a feeder that has never
been told the time must wait rather than guess.

To drop a single retained topic, publish an empty message to it:

```sh
docker compose exec mosquitto mosquitto_pub -h localhost -u feeder -P feeder-dev \
  -r -n -t 'homeassistant/button/feeder_<id>/feed/config'
```

Home Assistant's own state lives in `dev/homeassistant/` as ordinary files, so
`down -v` leaves it alone. Delete that directory to start Home Assistant from
scratch.

## Pointing the firmware at it

The Wi-Fi and broker credentials live in a git-ignored `cfg.toml` at the
repository root. For this stack:

```toml
mqtt_host     = "192.168.68.108"   # ipconfig getifaddr en0
mqtt_port     = 1883
mqtt_user     = "feeder"
mqtt_password = "feeder-dev"
```

`cfg.toml` feeds two different things, and which one a board is using matters
when something does not connect:

```sh
./dev/provision.sh     # writes these into the board's flash, once. Nothing compiled.
```

A provisioned board reads them from flash on every boot and keeps them across
reflashes — the console says `store: configured for ...`. Changing any of them
is `./dev/provision.sh` again, with no rebuild.

A board with **no** record does not fall back to anything: it says
`store: no record yet, going to setup` and raises its own Wi-Fi network.
Nothing is compiled into the binary, so there is no third outcome.

## When the ESP32 cannot connect

Work down this list. The broker log is the fastest oracle, because it
distinguishes "never arrived" from "arrived and was rejected".

```sh
docker compose logs -f mosquitto
```

- **Nothing in the log at all.** The packets are not reaching the Mac. The LAN
  address changed, the ESP32 is on a different network, or macOS is blocking
  incoming connections. Check that the Mac and the feeder are on the same
  subnet, and confirm the port is open with `nc -z <lan ip> 1883`.
- **`New connection` then `not authorised`.** Credentials are wrong. They come
  from flash, so fix `cfg.toml` and re-run `./dev/provision.sh` — a rebuild
  changes nothing.
- **Connects and drops in a loop.** Two units are using the same client id, so
  each kicks the other off. The id derives from the MAC, so this means the
  derivation is broken rather than the network.
- **Connects, but Home Assistant shows nothing.** The firmware is not
  publishing discovery, or the payload is truncated. Watch
  `homeassistant/#` and compare against the `ha-mqtt-discovery` skill.

Note that the 2.4 GHz band is the only one the ESP32-C6 uses for this project's
purposes. A Mac on a 5 GHz-only guest network is on a different subnet more
often than people expect.
