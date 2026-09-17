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

`bootstrap.sh` defaults to user `feeder` and password `feeder-dev`. Pass
`--user` and `--password` if you prefer your own. The password file is
git-ignored.

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

It comes from DHCP and **will** change. When it does, every unit provisioned
before the move keeps pointing at an address that now belongs to something
else, and each one sits flashing red twice — which is correct behaviour for
"no broker" and indistinguishable from a broker that is genuinely down. This
has already cost a session.

So do not write it down. `cfg.toml` ships with

```toml
mqtt_host = "auto"
```

which `./dev/provision.sh` resolves to this machine's current address when it
builds the record — from the default route's interface, not a hardcoded `en0`.
The value stored in flash is always a literal IPv4, because `mqtt.rs` parses it
with `Ipv4Addr::from_str` and there is no resolver on the device.

Point one unit somewhere else without editing the file:

```sh
./dev/provision.sh --host 192.168.68.126     # the Pi
```

`auto` is a development convenience. For anything permanent, give the broker's
machine a DHCP reservation and write that address down instead.

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

The Pi runs Raspberry Pi OS with Docker, so the procedure is the one above:
copy the file into the directory mounted as Home Assistant's `/config`, include
the packages directory, restart. `cat_feeder.yaml` goes over **unchanged** —
nothing in it names a host, which is why it is tracked in this repo rather than
configured per machine.

```sh
scp homeassistant/packages/cat_feeder.yaml <pi>:~/ha/config/packages/
```

A Home Assistant that was set up through the UI has no `homeassistant:` block in
its `configuration.yaml` at all, and without one the packages directory is never
read. Add it:

```yaml
# ha/config/configuration.yaml
homeassistant:
  packages: !include_dir_named packages
```

Then restart Home Assistant and watch the Pi's broker. This needs no feeder, and
`--password` too if the feeder user there differs from the dev stack's:

```sh
./dev/watch.sh --host <pi> 'feeder/time' 'feeder/schedule'
```

A line a minute on `feeder/time` says the Pi's half is done.

**Read the offset on that line before believing it.** The feeders apply the
wall-clock fields straight from `feeder/time` without converting — see *MQTT
contract* in CLAUDE.md — so a Home Assistant on the wrong zone publishes a
payload that is still entirely valid with every meal moved by an hour. Mounting
`/etc/localtime` into the container is the usual way to get this right and does
not need a `TZ` variable, but note that it is not quite the thing being checked:
what renders `{{ now() }}` is Home Assistant's own `time_zone`, taken from the
system zone at onboarding and kept in `.storage/` rather than in a file. The two
normally agree. The offset on the wire is what proves it.

Two more differences from the stack above, both properties of the Pi's compose
file rather than of the broker:

- **Home Assistant with `network_mode: host`** reaches Mosquitto at `localhost`,
  not at `mosquitto`. The container-name address in *The broker has three
  different addresses* is a property of this stack's bridge network only.
- **A bind-mounted Mosquitto data directory** still needs `persistence true` in
  its config, or retained messages are lost on every broker restart. Most come
  straight back, because the package republishes the time each minute and the
  schedule on restart — but `feeder/<id>/paused` does not, and a paused feeder
  silently resuming is the one state change in this system that nothing alarms
  about.

⚠️ **Do not point one feeder at both stacks.** Every piece of persistent state
in this design is a retained message, so a unit moved back to the Mac picks up
whatever *that* broker last held — quite possibly a schedule from last week,
which is indistinguishable from a current one. Each unit points at one broker,
and changing it is a `./dev/provision.sh` run rather than something that can
happen by accident.

What the package sets up:

| Automation | When | Publishes |
|---|---|---|
| publish the time | every minute, on restart, and on `feeder/time/request` | `feeder/time`, retained |
| publish the schedule | on restart, or the `cat_feeder_republish_schedule` event | `feeder/schedule`, retained |
| pause when away | `schedule.cat_feeder_active` changes | `feeder/<id>/paused` per unit, retained |

Plus `script.cat_feeder_feed_all`, which publishes one `feeder/all/feed` so all
three turn at the same instant rather than being staggered by three round
trips.

**Not included, but easy to add: a warning when a unit stays paused.** A
forgotten pause is the one state where the cats do not eat and nothing else
alarms, so an automation firing after 48 hours on `switch.cat_feeder_<id>_paused`
is an obvious guard. It is deliberately absent here because of how these feeders
are used: the schedule is paused precisely when somebody is home to feed by
hand, which is most of the time, so the notification would fire on the normal
case and be trained away. It is worth adding for the opposite pattern — a
feeder that normally runs unattended, where a pause really is an accident.

**`TZ: Europe/Rome` in `compose.yaml` is load-bearing.** Home Assistant owns the
clock, the firmware reads the wall-clock fields and does not apply the offset,
and the container defaults to UTC. Without that variable every meal lands an
hour or two out while everything still looks healthy. The feeder prints the
offset it received at startup — `clock: live time 2026-09-18T00:07:18+02:00,
schedule armed` — which is the only place the mistake shows.

**The `mqtt` trigger on `feeder/time/request` is what makes a feeder start
quickly.** A unit only arms its schedule on a live time, and asks for one as the
last step of connecting; without that trigger it waits for the next minute
boundary instead, which is up to a minute of a boot spent doing nothing. Nothing
breaks without it — see *Asking for the time instead of waiting for it* in
CLAUDE.md — but a unit repointed at a Home Assistant that has not got the
package gets the slow path back.

To change feeding times, edit `meals` in the package, copy it in again, and
restart. Verified end to end: a slot published this way fired at exactly its
time, and the unit reported `"last_fed":"2026-09-15T19:56:00+02:00"`.

## Everyday commands

```sh
./dev/watch.sh                          # tail feeder/# and homeassistant/#
./dev/watch.sh 'feeder/+/state'         # one filter instead
./dev/watch.sh --host <pi>              # ...against the Pi's broker instead

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
