# cat-feeder

Replacement electronics for three commercial automatic cat feeders, so all
three dispense at the same instant.

The original board in each feeder is removed. The mechanics are kept: a 5 V
geared motor and a microswitch on the output hub, which clicks once per portion
dispensed. An ESP32-C6 running Rust firmware drives the motor and takes its
orders from Home Assistant over MQTT.

Home Assistant asks for *portions*; each unit turns as many *clicks* as its own
mechanism needs, because the three feeders are not all the same model.

There is no real-time clock and no NTP. Home Assistant publishes the time and
the feeding schedule as retained MQTT messages, and each feeder keeps them in
RAM. The broker is the only thing that remembers *when to feed*, which is why a
feeder that loses power and finds no broker waits rather than guessing.

Flash holds one thing only: how to reach the broker, plus each unit's own
mechanical calibration. Those are the facts the broker cannot supply, because
they are how a unit reaches it in the first place — and because the three
feeders are not all the same model.

## Status

Firmware is partway through the roadmap in [CLAUDE.md](CLAUDE.md).

| | |
|---|---|
| Boots, logs over serial | working |
| Wi-Fi, DHCP | working |
| MQTT connect, auth, last will, retained availability | working |
| State topic | working, real values |
| Debounced switch, feeding logic, jam detection | working, against a logging fake motor |
| Home Assistant discovery, commands, pause | working |
| Schedule, clock, double-feed guard | working |
| Home Assistant automations publishing time and schedule | working |
| Status LED on the onboard WS2812 | working, verified by eye |
| Outside button: hold to arm, tap to feed | working |
| Per-board provisioning from the host (`dev/provision.sh`) | working |
| Per-unit mechanical calibration | working, defaults until measured |
| Driving the actual motor | **not started** — no DRV8833 yet |
| Setup over the unit's own Wi-Fi | working, driven from a phone |

Nothing has run on a production board yet. All of the above was verified on the
Waveshare ESP32-C6-DEV-KIT-N8, with a bench button standing in for the hub
microswitch and log lines standing in for the motor.

The three Zeros are on the bench, and the Raspberry Pi 5 runs both containers,
though Home Assistant there is not set up yet and the feeders still point at the
Mac's Docker stack. The DRV8833 is the only part still outstanding, and it is
what blocks driving a real motor.

## Hardware

Per feeder:

- Waveshare ESP32-C6-Zero. Development happens on an ESP32-C6-DEV-KIT-N8.
- DRV8833 H-bridge breakout. Its `nSLEEP` pin must be driven high or the motor
  will not turn.
- The feeder's original 5 V motor, 8 rpm, and its hub microswitch.
- A 220 µF capacitor across 5 V and ground beside the driver. Without it the
  motor's inrush browns out the ESP32 on start.
- 5 V from the feeder's original USB port, 1 A or better. No batteries.

## Getting started

Rust stable with the `riscv32imac-unknown-none-elf` target, plus
[espflash](https://github.com/esp-rs/espflash). No `espup` needed, because the
C6 is RISC-V.

```sh
cargo install espflash
cp cfg.toml.example cfg.toml     # then fill in Wi-Fi and broker details
./dev/bootstrap.sh               # once, creates the dev broker password
docker compose up -d             # Mosquitto on 1883, Home Assistant on 8123
./dev/provision.sh               # once per board, writes cfg.toml into flash
cargo run                        # build, flash, and open the serial monitor
```

`cfg.toml` is git-ignored. `dev/provision.sh` writes its values straight into
the board's `nvs` partition, which an application reflash never touches — so a
board is provisioned once and keeps its settings across every `cargo run`, with
nothing compiled in and nothing re-seeded at boot.

It also carries this unit's mechanical calibration, so three feeders that are
not the same model can run one binary:

```sh
./dev/provision.sh --detent-ms 900 --portion-scale 133
```

**Nothing is compiled in.** A board with no record does not fall back to
anything — it raises its own Wi-Fi network and asks to be configured. The
console says `store: configured for ...` or `store: no record yet, going to
setup`, and there is no third answer.

A healthy boot looks like this:

```
INFO (277)   - board: devkit, id=db0260
INFO (11670) - wifi: connected, ip=192.168.68.123/24
INFO (11834) - mqtt: connected, id=feeder_db0260
INFO (11924) - mqtt: discovery published
INFO (11944) - mqtt: online
INFO (12061) - mqtt: subscribed
INFO (12578) - clock: started, 2026-09-15T21:45:00+02:00 (retained; waiting for a live time)
INFO (12588) - schedule: 2 slots
INFO (34593) - clock: live time 2026-09-15T21:46:00+02:00, schedule armed
```

The device id comes from the MAC, so one binary flashes all three units and
they still address distinct MQTT topics.

The gap before `schedule armed` is deliberate, not a fault. A retained
`feeder/time` is whatever the broker last stored, and if Home Assistant has
stopped it can be any age; the schedule waits for a live message before
trusting the clock. Up to a minute of this is normal. A unit that *never* prints
`schedule armed` will never feed on schedule — check Home Assistant is running.

The board flashes both boards from one source:

```sh
cargo run                                              # dev kit
cargo run --no-default-features --features board-zero  # a Zero
```

`--no-default-features` is not optional there; Cargo features are additive, and
asking for both boards fails in esp-println's build script.

To watch what the firmware is saying:

```sh
./dev/watch.sh
```

The local stack, including the three different addresses the broker answers on
and the Home Assistant automations that publish the time and the schedule, is
documented in [dev/README.md](dev/README.md).

## Setting up a feeder

> **Works today**, driven end to end from a phone: the unit raises its network,
> hands out an address, serves the form, saves what you type and reboots into
> it. `./dev/provision.sh` still configures a board over USB, which stays the
> quicker route while a unit is on the bench.

A feeder is set up from a phone, with no laptop and no toolchain, because the
case that actually hurts is not first boot — it is the
Wi-Fi password changing across three units already screwed into place.

**Once per project — pick a salt.** *Works today.*

```sh
echo "ap_secret    = \"$(openssl rand -hex 16)\"" >> cfg.toml
```

Each unit's setup password is derived from this salt and its device id, so it
differs per unit and cannot be worked out from the MAC the unit broadcasts.
Without it the password would be public, and WPA2 does not protect a session
from someone who knows the passphrase — including the session where you type
your home Wi-Fi password into the form.

**Once per unit — print a sticker.** *Works today.*

```sh
./dev/ap-password.sh db0260
# cat-feeder-db0260    55KA-G8H6-9NMQ
```

Pass several ids for all three at once. The id is the last three bytes of the
station MAC, printed at boot and by `espflash board-info`. Stickers can be made
before a unit is ever powered on, which is the point of the derivation being
reproducible off the device.

**Then, per unit.**

1. Hold the reset button on the outside of the case **while plugging the unit
   in**. It erases its stored configuration, which is the one and only way into
   setup. It is a power-on gesture rather than a runtime one so that it cannot
   happen by accident: the same button feeds, and separating the two by hold
   duration alone would mean a beat too long wipes a working feeder.
2. Join `cat-feeder-<id>` from a phone, using the password on the sticker.
3. Browse to `http://192.168.4.1`.
4. Fill in Wi-Fi and broker details, save.
5. The unit reboots onto your network and appears in Home Assistant by itself.

A brand-new unit skips step 1: fresh flash has no configuration, so it comes up
in setup mode on its own.

With a cable to hand, `./dev/provision.sh` does the same job without any of
this — it writes the record directly. The access point is for the case where
the units are already installed and a laptop is not.

There is deliberately no automatic fall back into setup after a failed
connection. A router rebooting for five minutes must not drop a working feeder
into setup mode and stop it feeding — the button makes that a decision rather
than an accident.

## The LED and the button

Each unit has an RGB LED and one button on the outside of the case. Between them
they cover the things Home Assistant cannot tell you — because the failures that
matter most are the ones where the unit cannot reach Home Assistant at all.

| LED | Meaning |
|---|---|
| red, green, blue at power-on | self-test. Proves the LED works, and that its colours are the right way round |
| **solid** red | jammed. Something is stuck; go and look |
| **solid** white | feeding |
| cyan, twice a second | the button is armed — a tap will dispense |
| red ×1 every 3 s | no Wi-Fi. Check the router or the credentials |
| red ×2 every 3 s | no broker. Check the broker address, or Mosquitto |
| red ×3 every 3 s | no trusted time, so **this unit will not feed on schedule**. Check Home Assistant is publishing |
| amber ×1 every 5 s | paused |
| green ×2, then dark | all well |

**Dark is the healthy state.** If lit were normal, lit would carry no
information and nobody would look at it. Count the flashes rather than judging
the colour: one, two and three point at three different things to fix, and
counting works across a dark room and for a colour-blind reader.

Red ×3 for up to a minute after a reboot is normal — the unit is waiting for
Home Assistant's next time publish. It only means something if it stays.

The button:

| Gesture | Effect |
|---|---|
| hold 2 s | arm it. The LED blinks cyan |
| tap while armed | feed one portion. Taps refresh the window |
| nothing for 10 s | locks again |
| held while plugging in | erase the configuration |

Arming exists because **a button on a cat feeder that dispenses food when
pressed is a button cats will learn to press.** Recess it as well; needing a
fingertip defeats a paw outright.

## MQTT

```
feeder/<id>/availability   online | offline          retained, last will
feeder/<id>/feed           <portions>                manual feed
feeder/all/feed            <portions>                all units at once
feeder/<id>/paused         ON | OFF                  retained, pauses the schedule
feeder/schedule            [{"time":"08:00","portions":2}]   retained, from HA
feeder/time                "2026-09-14T08:00:00+02:00"       retained, from HA
feeder/<id>/state          {"feeding":…,"jammed":…,"paused":…,"last_fed":…}
```

Each unit publishes Home Assistant discovery configs on connect, so a feeder
shows up as one device with a feed button, a pause switch and a jam sensor. No
YAML on the Home Assistant side.

## Layout

Pure logic sits above the hardware gate in `src/lib.rs` and is tested on the
host; anything touching a peripheral is below it and is verified on the console.

```
src/
  bin/main.rs     peripherals, tasks, executor

  feeder.rs       pure: align, count, brake, jam timeout, per-unit timings
  portions.rs     pure: the pending-click counter, its cap, portions -> clicks
  schedule.rs     pure: clock, schedule, the double-feed guard
  provisioning.rs pure: the flash record, setup credentials and form
  sha256.rs       pure: shared with dev/ap-password.sh

  button.rs       pure: what a press of the outside button means
  indicator.rs    pure: what the status LED shows, and when

  board.rs        pin map and board identity, per Cargo feature
  switch.rs       debounced click stream
  led.rs          the onboard WS2812, over RMT
  motor.rs        MotorDriver, the DRV8833, and a logging stand-in
  mqtt.rs         connection, last will, discovery, commands, state
  store.rs        reads and writes the record in the nvs partition
  wiring.rs       what the tasks share
  config.rs       Config from the flash record, and the MAC-derived device id
  dhcp.rs         pure logic: where a DHCP reply goes, and a MAC's spelling
  setup.rs        setup mode: the unit's own network, DHCP, and the sockets

build.rs          reads cfg.toml into the build
dev/              local Mosquitto and Home Assistant, plus the scripts
homeassistant/    the automations that publish time and schedule
CLAUDE.md         design decisions and the contract the firmware implements
```

```sh
cargo test --lib --target "$(rustc -vV | awk '/^host:/{print $2}')"
```

The explicit target is not optional: `.cargo/config.toml` points cargo at the
board, so a plain `cargo test` builds the tests for the ESP32-C6 and fails to
link.

`CLAUDE.md` is worth reading before changing anything. It records what was
decided and why, including the parts that look arbitrary: why feeding counts
switch edges instead of levels, why a missed meal is preferable to a double
one, and why there is no local clock.
