# cat-feeder

Replacement electronics for three commercial automatic cat feeders, so all
three dispense at the same instant.

The original board in each feeder is removed. The mechanics are kept: a 5 V
geared motor and a microswitch on the output hub, which clicks once per portion
dispensed. An ESP32-C6 running Rust firmware drives the motor and takes its
orders from Home Assistant over MQTT.

Home Assistant asks for *portions*; each unit turns as many *clicks* as its own
mechanism needs, because the three feeders are not all the same model.

Each unit owns its clock and its schedule. A DS3231 real-time clock with a coin
cell keeps the time through a power cut, and Home Assistant's `feeder/time`,
published every minute, keeps it corrected; there is no NTP. The schedule is
sent to a unit deliberately — a new unit starts with no meals and does not feed
until given some — and the unit keeps it in its own flash. So a feeder that
loses power comes back feeding, with or without a broker, as long as its RTC
was set. One whose RTC was never set, or whose coin cell is flat, waits rather
than guessing.

Flash holds three things: how to reach the broker, each unit's own mechanical
calibration, and its meals. The first two are facts the broker cannot supply,
because they are how a unit reaches it in the first place — and because the
three feeders are not all the same model.

## Status

Firmware is partway through the roadmap in [CLAUDE.md](CLAUDE.md).

| | |
|---|---|
| Boots, logs over serial | working |
| Wi-Fi, DHCP | working |
| MQTT connect, auth, last will, retained availability | working |
| State topic | working, real values |
| Debounced switch, feeding logic, jam detection | working, against a real bridge |
| Home Assistant discovery, commands, pause | working |
| Schedule, clock, double-feed guard | working |
| DS3231 real-time clock: arms the schedule at boot, with no Home Assistant | working, verified on the Zero |
| The unit owns its schedule, in flash; a new unit starts blank | working, verified on the Zero |
| Home Assistant: an automation publishing the time, a script sending the schedule | working |
| Status LED on the onboard WS2812 | working, verified by eye |
| The knob: turn for info pages, hold for a menu, tap `Feed` to feed | working on a Zero; the info pages are host-tested but not yet seen on the panel |
| Per-board provisioning from the host (`dev/provision.sh`) | working |
| Per-unit mechanical calibration | working, defaults until measured |
| Driving the actual motor | working — align, count and brake watched on a bench motor |
| OLED: driver, probed address, a screen that sleeps | working, on a 0.96" 128×64 SSD1315 |
| Setup over the unit's own Wi-Fi | working, driven from a phone |

Most of that was verified on the Waveshare ESP32-C6-DEV-KIT-N8, with a bench
button standing in for the hub microswitch. The first production board — a
Zero, id `99177c` — now runs as well, on a breadboard with the driver, the
switch, the button and a panel: it drives the bridge and draws on the glass.

What has *not* happened is any of it turning a feeder's own mechanism. The
motor has been watched aligning, counting and braking on the bench, but a bare
shaft, so the detent interval each unit needs is still unmeasured and which way
the hub turns under `IN1=1, IN2=0` is still unobserved.

Every part has now arrived — the three Zeros, the DRV8833, a display — and the
feeders still point at the development stack that runs in Docker on a laptop,
rather than at an always-on Home Assistant. What is left before a
feeder runs on its own hardware is soldering and CAD, not ordering: the
electronics go in a separate 3D-printed case rather than into each feeder's own
LCD window, so one enclosure design serves all three — including the odd one
out — and each feeder needs only a hole in its bottom shell for the cables.

## Hardware

Per feeder:

- Waveshare ESP32-C6-Zero. Development happens on an ESP32-C6-DEV-KIT-N8.
- DRV8833 H-bridge breakout. Its `nSLEEP` pin must be driven high or the motor
  will not turn.
- The feeder's original 5 V motor, 8 rpm, and its hub microswitch.
- A 220 µF capacitor across the driver's 5 V and ground. Without it the
  motor's inrush browns out the ESP32 on start.
- 5 V from the feeder's original USB port, 1 A or better.
- Optionally the feeder's own AA batteries as a backup, joined through a dual
  Schottky (MBRF2045CT) so USB never charges them. Roughly a day of runtime,
  estimated rather than measured.
- A perfboard to carry it all: `pcb.diy`, drawn in DIY Layout Creator, with
  `pcb.png` as the picture to solder from.

## Getting started

Rust stable with the `riscv32imac-unknown-none-elf` target, plus
[espflash](https://github.com/esp-rs/espflash). No `espup` needed, because the
C6 is RISC-V.

```sh
cargo install espflash
cp cfg.toml.example cfg.toml     # then fill in Wi-Fi and broker details
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
console says `store: configured for ...`, or `store: no record yet, going to
setup` — or, after a reset, `store: no network in the record (calibration
kept), going to setup` — and nothing else.

A healthy boot looks like this:

```
INFO - board: zero, id=99177c
INFO - rtc: DS3231 holds 2026-09-25T19:03:12, running since last set, 26.00 C
INFO - schedule: 2 meals from flash
INFO - clock: RTC time 2026-09-25T19:03:12, schedule armed
INFO - wifi: connected, ip=192.168.68.115/24
INFO - mqtt: connected, id=feeder_99177c
INFO - mqtt: discovery published
INFO - mqtt: online
INFO - mqtt: subscribed
INFO - mqtt: asked for the time
```

With the RTC set, the schedule arms about 1.4 s after power-on, before Wi-Fi is
up — the unit does not need Home Assistant to feed. A unit with no meals says
`schedule: none stored; this unit will not feed until given one` instead, and
shows `NO MEALS SET` on its panel.

The device id comes from the MAC, so one binary flashes all three units and
they still address distinct MQTT topics.

`mqtt: asked for the time` is a publish to `feeder/time/request`, sent once per
connection after subscribing. The schedule only starts on a *live* time — a
retained one may be any age if Home Assistant has stopped — and without asking,
a unit waits for the next minute boundary, up to a full minute of doing nothing.

For a unit whose RTC is set, this only keeps the clock corrected. For one whose
RTC is not — its console says `oscillator stopped since last set: not trusted` —
the live answer is what arms it, so `clock: live time ..., schedule armed`
normally follows within a second. If instead the console shows `clock:
started, ... (retained; waiting for a live time)` and stops there, nobody
answered: the broker is up but Home Assistant is not publishing, and that unit
will not feed on schedule until it does.

The board flashes both boards from one source:

```sh
cargo run                                              # dev kit
cargo run --no-default-features --features board-zero  # a Zero
```

`--no-default-features` is not optional there; Cargo features are additive, and
asking for both boards fails in esp-println's build script.

### Headless

A unit can be built with no knob and no panel — the perfboard in `pcb.diy` is
one:

```sh
./dev/flash.sh --board zero --headless
# or: cargo run --no-default-features --features board-zero,headless
```

It leaves out the menu, the encoder and the display, and the console says
`board: headless, no knob and no panel`. Everything else is the same unit:
feeding, the schedule, the RTC, MQTT and the admin page. What changes:

- **It is configured over the network only** — its setup network, its admin
  page and Home Assistant. Setup mode shows its password only on the serial
  console, so print the sticker from `./dev/ap-password.sh` before the unit
  goes into a case; it is the only way to join it.
- **Its one reset is BOOT held for five seconds** — there is no knob to hold
  through power-on, and GPIO3 is not even read. Put a pinhole over the button.
- GPIO3–GPIO5 are free.

To watch what the firmware is saying:

```sh
./dev/watch.sh
```

The local stack, including the three different addresses the broker answers on
and the Home Assistant package that publishes the time and sends the schedule, is
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

1. Put the unit into setup mode, in one of two ways. Either forgets the
   network settings and keeps the meals, the calibration and the timezone.
   - **Hold the knob's click while plugging the unit in**, for three seconds.
     It is a power-on gesture rather than a runtime one so that it cannot
     happen by accident: the same click opens the feeding menu, and
     separating the two by hold duration alone would mean a beat too long
     wipes a working feeder.
   - **Hold BOOT for five seconds while it runs**, through a pinhole in the
     case. The LED flashes fast blue while it counts; let go early and
     nothing changes. On a [headless](#headless) unit this is the only way.
2. Join `cat-feeder-<id>` from a phone, using the password on the sticker. A
   unit with a screen fitted is also *meant* to show the network name, its
   password and the address for as long as it waits — that is written but has
   not yet been seen on a panel, so take the sticker as the one that works.
3. Browse to `http://192.168.4.1`.
4. Fill in Wi-Fi and broker details, save.
5. The unit reboots onto your network and appears in Home Assistant by itself.

A brand-new unit skips step 1: fresh flash has no configuration, so it comes up
in setup mode on its own.

With a cable to hand, `./dev/provision.sh` does the same job without any of
this — it writes the record directly. The access point is for the case where
the units are already installed and a laptop is not.

**Once it is running, it has a page of its own** at `http://<its address>/` —
the address is on the knob's `WI-FI` page, and Home Assistant links to it as
*Visit device*. Log in with any username and the same sticker password. It
shows the clock and the next meal, feeds on demand, edits the eight meals,
sets the clock (one button takes your phone's time) and its timezone, so it
keeps summer time even with no Home Assistant, sets or measures the detent
interval and portion scale, and changes the Wi-Fi or broker (which
restarts the unit). Stored passwords are never shown;
leave a password box empty to keep it.

There is deliberately no automatic fall back into setup after a failed
connection. A router rebooting for five minutes must not drop a working feeder
into setup mode and stop it feeding — the reset makes that a decision rather
than an accident.

## The LED and the button

Each unit has an RGB LED, the knob on the outside of the case, and the
board's BOOT button behind a pinhole. Between them they cover the things Home
Assistant cannot tell you — because the failures that matter most are the ones
where the unit cannot reach Home Assistant at all. A headless unit has only
the LED and BOOT.

| LED | Meaning |
|---|---|
| red, green, blue at power-on | self-test. Proves the LED works, and that its colours are the right way round |
| blue, fast (every 0.4 s) | BOOT is held: at five seconds the network settings go. Let go to cancel |
| **solid** red | jammed. Something is stuck; go and look |
| **solid** white | feeding |
| cyan, twice a second | the knob's menu is open — a tap on `Feed` will dispense |
| red ×1 every 3 s | no Wi-Fi. Check the router or the credentials |
| red ×2 every 3 s | no broker. Check the broker address, or Mosquitto |
| red ×3 every 3 s | no trusted time, so **this unit will not feed on schedule**. Check Home Assistant is publishing |
| amber ×1 every 5 s | paused |
| green ×2, then dark | all well |

**Dark is the healthy state.** If lit were normal, lit would carry no
information and nobody would look at it. Count the flashes rather than judging
the colour: one, two and three point at three different things to fix, and
counting works across a dark room and for a colour-blind reader.

Red ×3 should be a flicker on the way up, not something you can count: the unit
asks for the time on connecting and Home Assistant answers within a second. Red
×3 you can actually sit and count means nobody answered — Home Assistant is down
or its publish-the-time automation is missing — and that unit will not feed on
schedule until it is fixed. A unit whose RTC is set arms from it at boot and
never shows red ×3 at all.

The knob — a rotary encoder whose click is the outside button:

| | Turn | Tap | Hold 2 s |
|---|---|---|---|
| **locked** | step the pages: home, Wi-Fi, broker, device | back to home | open the menu, on `Feed`. The LED blinks cyan |
| **menu open** | move the cursor | run the item: `Feed one portion`, `Pause`/`Resume schedule`, `Settings`, `Lock` | lock |
| **editing a setting** | change the value | save it; in force from the next feed | cancel |
| nothing for 10 s | | | locks again |
| held while plugging in, 3 s | | | forget the network settings |

The menu has to be opened by a hold because **a control on a cat feeder that
dispenses food is a control cats will learn to use.** Turning never dispenses.
A knob cannot be recessed the way a button can, so mount the case where a paw
has no footing.

`Settings` holds the clock, set by hand, this unit's portion scale and detent
interval — the two figures `dev/provision.sh --portion-scale` and `--detent-ms` set — and a
factory reset that asks `Keep` or `Erase` first. The factory reset erases the
credentials, the calibration *and the meals*; holding the click while plugging
in, or BOOT for five seconds, forgets only the network and keeps the meals,
the calibration and the timezone. A pause set from the menu
lasts until Home Assistant next restarts or its schedule helper changes; Home
Assistant is the authority.

## MQTT

```
feeder/<id>/availability   online | offline          retained, last will
feeder/<id>/feed           <portions>                manual feed
feeder/all/feed            <portions>                all units at once
feeder/<id>/paused         ON | OFF                  retained, pauses the schedule
feeder/<id>/schedule       [{"time":"08:00","portions":2}]   never retained, this unit's meals
feeder/all/schedule        [{"time":"08:00","portions":2}]   never retained, every unit's meals
feeder/<id>/schedule/state [{"time":"08:00","portions":2}]   retained, what the unit holds
feeder/time                "2026-09-14T08:00:00+02:00"       retained, from HA
feeder/time/request        <id>                      never retained, to HA
feeder/<id>/state          {"feeding":…,"jammed":…,"paused":…,"meals":…,"last_fed":…}
```

Each unit publishes Home Assistant discovery configs on connect, so a feeder
shows up as one device with a feed button, a pause switch, a jam sensor, and
eight *Meal n time* / *Meal n portions* pairs that edit its schedule, plus a
*Visit device* link to its admin page. No YAML on the Home Assistant side.

```
feeder/<id>/meal/<n>/time      08:00:00              never retained, meal n's time
feeder/<id>/meal/<n>/portions  2                     never retained, 0 switches it off
```

## Layout

Pure logic sits above the hardware gate in `src/lib.rs` and is tested on the
host; anything touching a peripheral is below it and is verified on the console.

```
src/
  bin/main.rs     peripherals, tasks, executor

  feeder.rs       pure: align, count, brake, jam timeout, per-unit timings
  portions.rs     pure: the pending-click counter, its cap, portions -> clicks
  schedule.rs     pure: clock, schedule, the double-feed guard
  provisioning.rs pure: the flash record, the setup network's identity, the form
  sha256.rs       pure: shared with dev/ap-password.sh

  button.rs       pure: what a press of the outside button means
  reset.rs        pure: BOOT held five seconds forgets the network
  indicator.rs    pure: what the status LED shows, and when
  display.rs      pure: the six lines the screen shows, and when it sleeps
  menu.rs         pure: what the knob's turns and click mean
  encoder.rs      pure: the knob's A/B levels into detents

  board.rs        pin map and board identity, per Cargo feature
  switch.rs       debounced click stream
  led.rs          the onboard WS2812, over RMT
  oled.rs         the SSD1306 panel, over async I2C
  motor.rs        MotorDriver, the DRV8833, and a logging stand-in
  mqtt.rs         connection, last will, discovery, commands, state
  ds3231.rs       pure: the DS3231's registers — time, OSF, EOSC, temperature
  rtc.rs          the DS3231 over the shared I2C bus
  i2c.rs          the one I2C bus, shared by the panel and the RTC
  store.rs        reads and writes the records in the nvs partition
  wiring.rs       what the tasks share
  config.rs       Config from the flash record, and the MAC-derived device id
  dhcp.rs         pure logic: where a DHCP reply goes, and a MAC's spelling
  setup.rs        setup mode: the unit's own network, DHCP, and the form
  http.rs         the sockets under both web servers
  admin.rs        pure: the admin page — login, forms, the page itself
  web.rs          the admin page on the house network
  discovery.rs    pure: the Home Assistant entities and their configs

build.rs          reads cfg.toml into the build
dev/              local Mosquitto and Home Assistant, plus the scripts
homeassistant/    the package that publishes the time and sends the schedule
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
one, and why the unit keeps its own clock and schedule.
