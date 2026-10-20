# cat-feeder

Replacement electronics for commercial automatic cat feeders: an ESP32-C6
running Rust firmware drives the feeder's own motor, counts portions on its
own hub switch, and takes its orders from Home Assistant over MQTT.

The feeder's original board comes out; its mechanics stay. What you get:

- **It feeds without a network.** Each unit keeps its meals in flash and its
  time on a battery-backed clock, so a power cut or a dead router does not
  cost a meal.
- **It never double-feeds.** A missed meal is preferable to a double one, and
  the firmware is built around that rule.
- **Home Assistant integration with no YAML to write.** A unit announces itself
  as a device with a feed button, a pause switch, a jam sensor and editable
  meal times. Several feeders can be fed at the same instant.

## Will it fit your feeder?

Open the feeder and look at the mechanism. You need:

- **A 5 V DC motor with a reduction gearbox** turning the dispensing hub, so
  the hub stops dead when the motor stops.
- **A microswitch on the output hub that clicks once per portion.** One click
  is one portion; that is the whole contract between the firmware and the
  mechanism.

An optical or Hall-effect sensor in place of the microswitch will **not** work
without firmware changes: the firmware counts falling edges on a switch with
a pull-up.

Two mechanisms are known to work. They look alike outside and are the same
design inside:

| You will see | Motor | Time between clicks | Wiring |
|---|---|---|---|
| Separate wires from the mechanism | DRF-W500CA, 5 V, 8 rpm | about 2 s | needs re-crimping into the board's connector order |
| A flat ribbon cable from the mechanism | HC 180-15180, 5 V, in its own gearbox | about 5.3 s | plugs straight into the board |

Both figures were measured with an empty hopper. A different feeder of the
same shape will very likely work; it needs two things measured, both done
from the unit itself once it is running:

- **The detent interval** — the time between clicks. *Run calibration* on the
  unit's admin page measures it. Do it with a **full hopper**: a loaded
  mechanism is slower, and calibrating empty gives false jam alarms on refill
  day.
- **The portion size** — what one click dispenses, by weight or by counting
  clicks into a spoon. If it differs between your feeders, set each unit's
  portion scale so the same meal dispenses the same amount everywhere.

## Parts, per feeder

| Part | Notes |
|---|---|
| Waveshare **ESP32-C6-Zero** | the production board (8 MB flash) |
| **DRV8833** H-bridge breakout | drives the motor |
| **DS3231** RTC module with a **LIR2032** cell | keeps time through power cuts. Power it from 3V3 |
| 220 µF 16 V electrolytic capacitor | stops the motor's inrush browning out the ESP32 |
| **MBRF2045CT** dual Schottky | joins USB and the feeder's own AA batteries, so USB never charges them |
| 1 kΩ resistor | guards the switch input |
| Perfboard, female headers, a 6-pin header (or JST XH) | the layout is [`hardware/pcb.diy`](hardware/pcb.diy) |
| 5 V USB adapter, 1 A or more | into the feeder's original USB socket |
| *Optional:* 2× 1 kΩ, a 2-pin female header, a JST XH 4-pin header; a 5 V WS2812, a push button and 100 nF on a small board | to bring the LED and BOOT outside the feeder |
| *Optional:* ESP32-C6-DEV-KIT-N8 | for development on a breadboard |

There is no screen and no knob: a unit is configured entirely over the
network, from a phone or Home Assistant, and all it has on the outside is its
LED and the board's BOOT button.

## Building it

The modules sit in female headers on a perfboard, which goes inside the
feeder, where its original board was. 5 V comes from the feeder's own USB
socket, with its AA compartment as a backup supply. The LED and the BOOT
button are on the board, so with the feeder closed they are out of sight and
reach unless you bring them out — the LED behind the old display window or a
hole, BOOT through a hole, or both on a small board cabled to the perfboard's
`Ext` connector. All of it is optional; see
[ADR-0029](docs/adr/0029-the-electronics-go-inside-the-feeder.md).

[**docs/hardware.md**](docs/hardware.md) is the builder's guide: the cable
pinout, the power path, the perfboard, the safety warnings, and the beep test
to run before first power. Run `./dev/pcb-check` after any change to the
layout and before soldering.

## Flashing

You need Rust stable (the `riscv32imac-unknown-none-elf` target is installed
by `rust-toolchain.toml`), [espflash](https://github.com/esp-rs/espflash), and
Python 3.11 or later for the scripts in `dev/` (`.python-version` names it, for
pyenv). No `espup`: the C6 is RISC-V.

```sh
cargo install espflash
cp cfg.toml.example cfg.toml
```

**Pick a salt once**, before flashing anything:

```sh
openssl rand -hex 16          # put the result in cfg.toml as ap_secret
```

Each unit's setup password and admin password are derived from this salt and
the unit's id. Without the salt they could be worked out from the MAC the unit
broadcasts. Changing it later invalidates every label already printed. It is
the only value compiled into the firmware: Wi-Fi and broker credentials are
never in the binary.

**Flash**, matching the build to the board in your hand:

```sh
./dev/flash --board zero
./dev/flash --board zero --port /dev/cu.usbmodemXXXX
```

`./dev/flash` builds, flashes and captures the console for 45 s. A Zero
binary on a dev kit, or the reverse, flashes fine and then prints nothing, so
check `--board`. `cargo run --no-default-features --features board-zero` is the
same build with an interactive monitor.

The unit's id is the last three bytes of its MAC, printed at boot
(`board: zero, id=a1b2c3`). One binary serves every unit.

**Print a label** for the outside of the feeder, with the unit plugged in:

```sh
./dev/label                 # reads the id from the connected board
./dev/label a1b2c3 d4e5f6   # or name the ids
```

It opens a page of 50 × 30 mm labels with the setup network's name, its
password and a QR code that joins it. It needs `qrencode`. The page holds the
passwords in the clear, so it goes to a temporary file. **The label is the
only way to learn a unit's password** short of a serial cable — print it
before the unit goes into its feeder. `./dev/ap-password <id>` prints
the same thing as text.

**Updating a unit that is already in its feeder** goes over the network, once
it has been flashed over USB with a version that supports it:

```sh
./dev/ota --address 192.168.1.123 --id a1b2c3 --board zero
```

`--address` is the unit's own IP: its device page in Home Assistant links to
it. The unit checks the image before switching to it, including that it was built with the same `ap_secret`, and restarts into
it. A new image that cannot reach the broker within about two minutes
(`CONFIRM_SECS`) is undone by the unit itself, so a bad update costs those
minutes, not a trip to the feeder.

## Setting up a feeder

A unit with no configuration raises its own Wi-Fi network and waits.

1. Join `cat-feeder-<id>` from a phone: scan the label's QR code, or type the
   password on it. The setup page opens by itself; if it does not, browse to `http://192.168.4.1`.
2. Pick your Wi-Fi from the list (or type it, for a hidden network), and fill
   in its password and your MQTT broker. The broker must be an **IP address**:
   the firmware has no DNS resolver. Give the broker's machine a DHCP
   reservation.
3. Press *Test and save*. The unit tries your Wi-Fi and logs in to the broker.
   Your phone drops off the setup network for a few seconds while it does;
   it normally rejoins by itself, and the setup page reopens on the answer. A
   wrong Wi-Fi password or a refused broker login comes back to the form, and
   nothing is saved. If only the broker failed — it is not running yet, say —
   *Save anyway* keeps the settings and the unit retries the broker on its own.
4. Once it works, the page shows the address the unit was given, which is
   where its admin page will be, and the unit reboots onto your network and
   appears in Home Assistant.

With a USB cable to hand, `./dev/provision` does the same without the setup
network: fill in Wi-Fi and the broker in `cfg.toml`, then

```sh
./dev/provision --host 192.168.1.10 --user <name> --password-file <path>
```

The setup network stays up until someone configures it; there is no timeout.
A unit that cannot reach its Wi-Fi does **not** drop back into setup by itself
— a router rebooting must not take a working feeder off its schedule. Going
back to setup is always deliberate (see [Resetting Wi-Fi](#resetting-wi-fi)).

### The admin page

Every configured unit serves a page at `http://<its address>/`. Home
Assistant links to it as *Visit device*, and setup showed it when it saved.
Log in with any username and the label's password.

It shows the clock, the next meal and the last feed, and lets you:

- feed now;
- pause or resume the schedule (kept across a reboot);
- set the eight meals;
- set the clock (one button takes your phone's time) and the timezone, so the
  unit keeps summer time on its own when Home Assistant is not around;
- set or measure the calibration — **Run calibration** turns five portions
  into the bowl and times them, then offers to save the result;
- change Wi-Fi or the broker, which restarts the unit;
- [factory reset](#factory-reset) it.

Stored passwords are never shown; leave a password box empty to keep it.

### First things to do with a new unit

1. **Check the motor turns the right way** with a feed, before anything is
   bolted down. If it runs backwards, swap the two motor wires.
2. **Calibrate** with a full hopper and a bowl underneath.
3. **Give it meals.** A new unit starts with none and **will not feed until
   given some** — its admin page says `0 a day` and its state reports
   `"meals":0`. That is deliberate: a unit never inherits meals nobody chose
   for it.

## Home Assistant

You need:

- **Mosquitto** with a user for the feeders. `persistence true` is
  recommended, though nothing depends on it: each unit republishes what it
  owns on every reconnect.
- **Home Assistant on the right timezone.** It publishes local time with its
  offset, and the feeders use the wall-clock fields as they arrive. An
  instance left on UTC moves every meal by the offset while everything looks
  healthy. The console prints the offset it received — check it.
- **The MQTT integration** configured against that broker.
- **The package**, [`homeassistant/packages/cat_feeder.yaml`](homeassistant/packages/cat_feeder.yaml),
  copied unchanged into Home Assistant's `packages/` directory, with this in
  `configuration.yaml` if it is not there already:

  ```yaml
  homeassistant:
    packages: !include_dir_named packages
  ```

Install the package **before** pointing a feeder at the broker. It publishes
the time every minute, and answers a unit that asks for it on connecting.

Each unit then appears by itself as a device, *Cat feeder &lt;id&gt;*, with:

| Entity | What it does |
|---|---|
| **Feed** button | one portion per press; presses accumulate while the motor runs |
| **Paused** switch | pauses the schedule. Manual feeds still work |
| **Jammed** binary sensor | on after a jam, off again after the next successful click |
| **Feeding** event | in the Activity log: meals served or skipped, feeds at the unit, jams |
| **Meal 1–8 time** and **portions** | the unit's schedule. Zero portions switches a meal off |
| *Visit device* | a link to the unit's admin page |

The package adds:

- **`script.cat_feeder_copy_schedule`** — set one feeder's meals, then run this
  to copy them to every other feeder, or to chosen devices, an area, a floor or
  a label. It refuses rather than copy from or to an offline unit.
- **`script.cat_feeder_feed_all`** — one broadcast, so every feeder turns at
  the same instant.
- **`schedule.cat_feeder_active`** — a weekly schedule helper. When it turns
  off, every online feeder is paused; when it turns on, they resume. Drag its
  blocks for a holiday at home.

No feeder id appears in the package: units are found in the device registry,
so a new one joins by itself.

**The unit is the authority on pausing.** It keeps its pause in flash, so it
survives a reboot, and the admin page pauses and resumes it with no Home
Assistant at all. Home Assistant's Paused switch and helper send
a command when used; a feeder that is offline at that moment misses it and
keeps what it holds — the switch shows which.

A paused feeder is the one state where the cats do not eat and nothing alarms.
If yours normally runs unattended, add an automation that warns when
`switch.cat_feeder_<id>_paused` has been on for a day or two.

The full MQTT contract is in [CLAUDE.md](CLAUDE.md).

## Using it

### The LED

| LED | Meaning |
|---|---|
| red, green, blue at power-on | self-test |
| blue, fast | BOOT is held: at five seconds the Wi-Fi settings go. Let go to cancel |
| **solid** red | jammed — go and look |
| **solid** white | feeding |
| blue, every 2 s | setup mode: waiting to be configured |
| red ×1 every 3 s | no Wi-Fi |
| red ×2 every 3 s | no broker |
| red ×3 every 3 s | no trusted time — **will not feed on schedule** |
| amber ×1 every 5 s | paused |
| green ×2, then dark | all well |

**Dark is healthy.** Count the flashes rather than judging the colour: one,
two and three point at three different things to fix. Solid means the
mechanism; blinking means the network.

### Recovering from a jam

A jam stops the motor, discards any portions still owed, and shows solid red.
Clear the obstruction, then **ask it to feed again** — the Feed button in Home
Assistant, or *Feed now* on the admin page. The first click clears the jam. No power cycle is
needed.

There is no reverse. A retry drives forward into whatever stopped it, so it
recovers a momentary stall; a real blockage needs a hand.

### Resetting Wi-Fi

**Hold BOOT for five seconds** while the unit runs — through a hole in the
feeder if you made one, otherwise with the feeder open; `./dev/provision`
over USB is the other way. It forgets the Wi-Fi and broker settings and reboots into setup mode;
**the meals, the calibration, the timezone and the pause are kept.** The LED
flashes fast blue while it counts; let go early and nothing changes.

### Factory reset

*Factory reset*, at the bottom of the admin page and of the setup page, erases
everything the unit was told — Wi-Fi and broker, calibration, meals, timezone
and pause — and restarts it in setup mode. The clock is kept: it lives in
the RTC, not in flash. Tick the box, then confirm. A unit that cannot reach your network gets there through the setup
page: hold BOOT for five seconds first. The admin page refuses while the unit
is feeding.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| red ×1 | Wi-Fi: wrong credentials, or out of range |
| red ×2 | broker unreachable: wrong address (it may have moved — use a DHCP reservation), wrong credentials, or Mosquitto down |
| red ×3 that lasts | no trusted time: the RTC lost power (flat or missing coin cell) **and** Home Assistant is not publishing the time. Install the package; check the coin cell |
| console: `clock: started, … (retained; waiting for a live time)` and nothing more | the broker is up but Home Assistant is not publishing `feeder/time` |
| online, dark LED, never feeds; `"meals":0` | the unit has no meals. Set them, or copy them from another feeder |
| amber ×1 | paused — resume it from the admin page or the Paused switch |
| console: `ignored a retained paused command` at every connect | a retained flag left on the broker by an older firmware or package. Clear it once: `mosquitto_pub -r -n -t feeder/<id>/paused`. A unit that was paused that way comes back **running** after the upgrade — pause it again from the admin page or the switch |
| every meal an hour or two out | Home Assistant's timezone is wrong (check the offset on `feeder/time`) |
| solid red | jammed — see [Recovering from a jam](#recovering-from-a-jam). Repeated false jams with a full hopper: recalibrate with it full |
| `rtc: nothing answered; running without one` | an open I²C line, or the DS3231 fitted on the wrong side. Beep SDA and SCL module pin to Zero pin |
| RTC cell flat within months | a CR2032 in a module that charges its cell, or the module on 5 V |
| motor turns the wrong way | swap the two motor wires (pins 4 and 5 of the header) |
| plugged in, nothing happens, laptop's USB port cuts out | the DRV8833's supply reversed on the board |
| nothing on the console after flashing | built for the wrong board: check `--board`, and the port |
| `schedule: stored record is unreadable` on a brand-new Zero | data left by the factory demo; harmless, goes once the unit is given meals |
| `store: no network in the record (calibration kept), going to setup` | the Wi-Fi was reset; set it up again |

The serial console says what the unit is doing at every step:
`./dev/capture` listens without reflashing or resetting the unit;
`--reset` captures a boot instead.

## For developers

- Host tests: `cargo test --lib --target "$(rustc -vV | awk '/^host:/{print $2}')"`
  — the explicit target is needed because cargo is pointed at the board.
- [CLAUDE.md](CLAUDE.md) — developer notes: architecture, the MQTT contract,
  conventions, and the code layout (also read by AI coding assistants).
- [docs/adr/](docs/adr/) — the design decisions and why they were made.
- [docs/roadmap.md](docs/roadmap.md) — open work.
- [dev/README.md](dev/README.md) — a local Mosquitto and Home Assistant in
  Docker, and the dev scripts.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT licence](LICENSE-MIT), at your option. This covers everything in the
repository, including the perfboard drawing and the Home Assistant package.

Unless you explicitly state otherwise, any contribution you intentionally
submit for inclusion in this work, as defined in the Apache-2.0 licence, is
dual licensed as above, without any additional terms or conditions.
