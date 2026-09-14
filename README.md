# cat-feeder

Replacement electronics for three commercial automatic cat feeders, so all
three dispense at the same instant.

The original board in each feeder is removed. The mechanics are kept: a 5 V
worm-gear motor and a microswitch on the output hub that clicks four times per
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
| State topic | working, values are mocked |
| Switch, motor, feeding | not started |
| Home Assistant discovery and commands | not started |
| Schedule and clock | not started |

Nothing has run on a production board yet. All of the above was verified on the
Waveshare ESP32-C6-DEV-KIT-N8.

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
INFO - board: devkit, id=db0260
INFO - wifi: connected, ip=192.168.68.123/24
INFO - mqtt: connected, id=feeder_db0260
INFO - mqtt: online
INFO - mqtt: state published {"feeding":false,...}
```

The device id comes from the MAC, so one binary flashes all three units and
they still address distinct MQTT topics.

To watch what the firmware is saying:

```sh
./dev/watch.sh
```

The local stack, including the three different addresses the broker answers on,
is documented in [dev/README.md](dev/README.md).

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

```
src/
  bin/main.rs   peripherals, tasks, executor
  config.rs     build-time config and the MAC-derived device id
  mqtt.rs       connection, last will, state publishing
build.rs        reads cfg.toml into the build
dev/            local Mosquitto and Home Assistant
compose.yaml
CLAUDE.md       design decisions and the contract the firmware implements
```

`CLAUDE.md` is worth reading before changing anything. It records what was
decided and why, including the parts that look arbitrary: why feeding counts
switch edges instead of levels, why a missed meal is preferable to a double
one, and why there is no local clock.
