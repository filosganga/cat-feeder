# cat-feeder

Replacement electronics for three commercial automatic cat feeders, so all
three dispense at the same instant.

The original board in each feeder is removed. The mechanics are kept: a 5 V
geared motor and a microswitch on the output hub that clicks four times per
revolution, one click per portion. An ESP32-C6 running Rust firmware drives the
motor and takes its orders from Home Assistant over MQTT.

There is no real-time clock, no NTP and nothing stored in flash. Home Assistant
publishes the time and the feeding schedule as retained MQTT messages, and each
feeder keeps them in RAM. The broker is the only thing that remembers, which is
why a feeder that loses power and finds no broker waits rather than guessing.

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
| Driving the actual motor | **not started** — no DRV8833 yet |
| Setup over the unit's own Wi-Fi | **partly built**, see below |

Nothing has run on a production board yet. All of the above was verified on the
Waveshare ESP32-C6-DEV-KIT-N8, with a bench button standing in for the hub
microswitch and log lines standing in for the motor.

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
cargo run                        # build, flash, and open the serial monitor
```

`cfg.toml` is git-ignored and compiled into the binary, so changing it needs a
rebuild rather than only a reflash.

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

> **Partly built.** The pieces below marked *works today* are in and tested.
> The access point itself is not, so for now a unit still takes its credentials
> from `cfg.toml` at build time. Roadmap step 9 in [CLAUDE.md](CLAUDE.md) tracks
> the rest.

The plan is that a feeder is set up from a phone, with no laptop and no
toolchain, because the case that actually hurts is not first boot — it is the
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

**Then, per unit.** *Not yet — this is what step 9 builds.*

1. Press and hold the reset button on the outside of the case. The unit erases
   its stored configuration and restarts with nothing, which is the one and only
   way into setup.
2. Join `cat-feeder-<id>` from a phone, using the password on the sticker.
3. Browse to `http://192.168.4.1`.
4. Fill in Wi-Fi and broker details, save.
5. The unit reboots onto your network and appears in Home Assistant by itself.

A brand-new unit skips step 1: fresh flash has no configuration, so it comes up
in setup mode on its own.

There is deliberately no automatic fall back into setup after a failed
connection. A router rebooting for five minutes must not drop a working feeder
into setup mode and stop it feeding — the button makes that a decision rather
than an accident.

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

  feeder.rs       pure: align, count, brake, jam timeout
  portions.rs     pure: the pending-portions counter and its cap
  schedule.rs     pure: clock, schedule, the double-feed guard
  provisioning.rs pure: the flash record, setup credentials and form
  sha256.rs       pure: shared with dev/ap-password.sh

  board.rs        pin map and board identity, per Cargo feature
  switch.rs       debounced click stream
  motor.rs        MotorDriver, the DRV8833, and a logging stand-in
  mqtt.rs         connection, last will, discovery, commands, state
  wiring.rs       what the tasks share
  config.rs       build-time config and the MAC-derived device id

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
