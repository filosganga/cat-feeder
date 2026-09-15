# What the console must show, per roadmap step

- [How to read this file](#how-to-read-this-file)
- [Step 1 — toolchain and blinky](#step-1--toolchain-and-blinky)
- [Step 2 — switch task](#step-2--switch-task)
- [Step 3 — motor and feed(n)](#step-3--motor-and-feedn)
- [Step 4 — Wi-Fi and MQTT](#step-4--wi-fi-and-mqtt)
- [Step 5 — schedule, time and the double-feed guard](#step-5--schedule-time-and-the-double-feed-guard)
- [Step 6 — the Zero boards](#step-6--the-zero-boards)
- [Step 7 — Home Assistant](#step-7--home-assistant)

## How to read this file

Each step lists the log lines that step must emit, the physical action that
triggers them, and the failure signatures worth recognising. The exact wording
is a proposal: implement it as written so this file stays usable as an
acceptance test, or change both the code and this file together.

Only step 1 has been observed on hardware. The rest are contracts, not
transcripts. When you first reach a step, replace its expected block with what
the console actually printed.

Every application line is formatted `LEVEL (ms) - message`, for example
`INFO (261) - Embassy initialized!`. The number is milliseconds since boot,
from `esp-println`'s `timestamp` feature, fed by `_esp_println_timestamp` in
`main.rs`. Lines shaped `I (nnn) boot:` come from the ESP-IDF bootloader, not
from this firmware.

**Use those timestamps.** Every timing rule in this project is checkable from
the console rather than guessed at: the 30 ms debounce, the 800 ms minimum
click spacing, ~1.9 s per portion, the 5 s jam timeout. To read gaps between
lines rather than absolute times:

```sh
espflash monitor --non-interactive --port "$ESPFLASH_PORT" \
  | sed 's/\x1b\[[0-9;]*m//g' \
  | awk -F'[()]' '{t=$2+0; if(NR>1) printf "%s  (+%d ms)\n",$0,t-p; else print; p=t}'
```

Observed on the dev kit for reference: boot to `Embassy initialized!` about
260 ms, Wi-Fi associated about 1.6 s, an address about 11.6 s. That last gap is
a lost first DHCP request and a retry, not negotiation.

## Step 1 — toolchain and blinky

Status: done and observed.

Action: `cargo run`, then watch.

```
I (198) boot: Loaded app from partition at offset 0x10000
I (198) boot: Disabling RNG early entropy source...
INFO - Embassy initialized!
INFO - Hello world!
INFO - Hello world!
```

`Hello world!` repeats once a second. Two consecutive lines more than a second
apart mean the executor is being starved.

Failure signature: the bootloader lines appear and nothing follows. The
application is running but its output is going nowhere. See the `esp-println`
entry in [troubleshooting.md](troubleshooting.md).

## Step 2 — switch task

Two passes. A push button on a breadboard is enough for the first one and is the
better place to start, because it separates wiring problems from mechanical
ones. The real hub comes second.

`switch.rs` debounces at 30 ms and reports **every** real edge. It does **not**
apply the 800 ms minimum spacing; that belongs to `feeder.rs`, where the motor
guarantees clicks cannot arrive faster than about 1900 ms. Getting this backwards
is the likeliest mistake in this step, and it shows up as a bench button that
ignores every second press.

### Pass 1, bench button

Action: flash, then press the button, including deliberately fast double
presses.

```
INFO - Embassy initialized!
INFO - switch: waiting for clicks on GPIO<n>
INFO - switch: click 1
INFO - switch: click 2
```

- **Every press counts, however fast.** Two presses 200 ms apart must produce
  two clicks. If the second is swallowed, the 800 ms spacing rule has been put
  in the switch stream instead of the feeder.
- One press giving several clicks means the 30 ms debounce is not working. Log
  raw edges at `debug` to see the bounce.
- Clicks with nothing touching the button mean the input is floating, so the
  internal pull-up is not enabled.
- Holding the button down must produce exactly one click, not a stream. The
  task counts falling edges, not the level.

### Pass 2, the real hub

Action: turn the output hub by hand, slowly, through one full revolution.

Exactly **four clicks per revolution**. That is the mechanical contract and the
real point of this step. Fewer than four means a missed edge; more means bounce
the debounce did not catch.

Run it twice, once with the hub parked on a detent so the switch starts pressed
and once parked between detents so it starts free. Both must give four. A run
that reports a click the instant the task starts is reading the level rather
than waiting for an edge.

## Step 3 — motor and feed(n)

Do this in two passes. First with the onboard RGB LED on GPIO8 standing in for
the motor, with no driver wired. Then with the DRV8833 connected.

Action: trigger a two-portion feed.

```
INFO - feed: start, portions=2
INFO - feed: aligned
INFO - feed: click 1/2
INFO - feed: click 2/2
INFO - feed: done, portions=2, elapsed=3.9s
```

Checks:

- Elapsed time is about 1.9 s per portion. Much faster means clicks are being
  counted from bounce rather than from detents.
- The motor brakes **on** the edge, so the hub parks in the same position every
  time. Mark the hub and confirm it lands identically across several feeds.
- With the LED standing in for the motor, the LED is on for the same interval
  the motor would run.

### The align phase

The hub normally rests with the switch already pressed, because that is where
the previous feed braked. Test both starting positions, because only one of them
exercises the alignment:

| Hub parked | Expected | Meaning |
|---|---|---|
| On a detent, switch pressed | `feed: aligned` immediately, then full portions | nothing to align |
| Between detents, switch free | a partial turn, then `feed: aligned`, then full portions | alignment ran |

Starting free and getting a first portion noticeably shorter than 1.9 s means
the align phase is missing and the first portion is a fraction of a turn.

Then force the bounce case. Trigger a feed with the hub parked exactly on the
edge, which is the normal resting position, and watch the first click:

```
INFO - feed: start, portions=1
INFO - feed: aligned
DEBUG - feed: edge at 40ms ignored, below 800ms minimum spacing
INFO - feed: click 1/1
INFO - feed: done, portions=1, elapsed=1.9s
```

A feed that completes in well under a second has counted the startup bounce as a
portion. The 800 ms minimum spacing is what prevents that, and it is separate
from the 30 ms debounce in `switch.rs`. Both must be present, in their own
modules: the debounce filters contact bounce everywhere, the spacing rule
rejects impossible-at-8-rpm edges and is only correct while the motor drives.

Cross-check against step 2. The same fast edges that `feeder.rs` rejects here
must still be counted by the bench button test, because `switch.rs` reports
them. If both tests pass, the rule is in the right place.

### Accumulation without stopping

Queue a second request while the motor is still turning, and listen rather than
read. The motor must **not** stop between portions:

```
INFO - feed: start, portions=1
INFO - feed: aligned
INFO - feed: pending=2
INFO - feed: click 1/1
INFO - feed: click 1/1
INFO - feed: done, elapsed=3.9s
```

One continuous 180° turn, not two starts. A audible stop and restart between
portions means the brake is happening on every loop iteration rather than only
when the queue is empty.

Jam path, forced by holding the hub still:

```
INFO - feed: start, portions=3
INFO - feed: aligned
INFO - feed: click 1/3
WARN - feed: no click for 5s, jammed
INFO - feed: motor braked, 2 pending portions discarded
```

The motor must stop. A jam that leaves the motor energised is a fire risk, not a
log-level problem.

Whatever was pending is dropped, not resumed. Feeding a queue into a jammed
mechanism is worse than losing a meal. Confirm that clearing the jam and
releasing the hub does not produce the two portions that were outstanding.

If the board resets the instant the motor starts, and the boot banner reappears
with a reset reason other than `POWERON`, the 220 µF capacitor across 5 V and
ground next to the DRV8833 is missing or too far from the driver.

Nothing moves at all: `nSLEEP` / `ULT` on the DRV8833 must be driven high. It is
not pulled up on the breakout.

## Step 4 — Wi-Fi and MQTT

Action: flash with credentials configured, watch the console and
`mosquitto_sub -v -t 'feeder/#' -t 'homeassistant/#'` side by side.

```
INFO - wifi: connecting to <ssid>
INFO - wifi: connected, ip=192.168.1.42
INFO - mqtt: connecting to 192.168.1.10:1883
INFO - mqtt: connected, id=feeder_<id>
INFO - mqtt: discovery published (button, switch, binary_sensor)
INFO - mqtt: online
INFO - mqtt: subscribed to feed, paused, schedule, time
```

Verify on the broker, not only on the console: the three retained configs under
`homeassistant/`, then `online` retained on `feeder/<id>/availability`. The
device must appear in Home Assistant as one device with three entities.

Then publish a manual feed and watch both sides:

```sh
mosquitto_pub -t 'feeder/<id>/feed' -m '1'
```

```
INFO - mqtt: rx feeder/<id>/feed = 1
INFO - feed: start, portions=1
INFO - feed: done, portions=1, elapsed=2.0s
INFO - mqtt: state published
```

Then the accumulation test, which is the part most likely to be wrong. Send
three presses back to back, faster than one feed cycle:

```sh
for i in 1 2 3; do mosquitto_pub -t 'feeder/<id>/feed' -m '1'; done
```

```
INFO - mqtt: rx feeder/<id>/feed = 1
INFO - feed: start, portions=1
INFO - mqtt: rx feeder/<id>/feed = 1
INFO - feed: pending=1
INFO - mqtt: rx feeder/<id>/feed = 1
INFO - feed: pending=2
INFO - feed: click 1/1
INFO - feed: done, portions=1, elapsed=2.0s
INFO - feed: start, portions=1
...
```

Count the clicks, not the log lines: the hub must turn **three** detents in
total. Two failure modes to watch for, both of which look fine in the log:

- Only one portion dispensed. The commands arriving during a feed were dropped
  instead of accumulating.
- The task deadlocks after the first feed. The counter is being read under a
  lock the feeder task still holds.

Then check the clamp by sending 20 presses:

```
WARN - feed: pending clamped to 10
```

Pull the power and confirm the broker shows `offline` on the availability topic:
that proves the last will was registered in the CONNECT packet.

Reconnect with the broker back up and confirm **no** feed happens on reconnect.
A feed at that moment means a `feed` message was published retained somewhere.

Failure signatures:

- `wifi: connected` but no IP: DHCP is not completing. The `embassy-net` stack
  needs the `dhcpv4` feature and a running `net` task.
- Connects then drops every few seconds: the same client id is in use by another
  unit, so the MAC-derived id is not actually unique.
- Entities missing in Home Assistant while `mosquitto_sub` shows the topics: the
  payload is malformed or truncated. See the `ha-mqtt-discovery` skill.

## Step 5 — schedule, time and the double-feed guard

Action: publish a retained time and schedule, then step the clock.

```
INFO - mqtt: rx feeder/time = 2026-09-14T07:59:30+02:00
INFO - clock: aligned, drift=-120ms
INFO - mqtt: rx feeder/schedule = 2 slots
INFO - schedule: next due 08:00, portions=2
INFO - schedule: slot 08:00 due, feeding 2
INFO - feed: done, portions=2, elapsed=3.9s
INFO - schedule: last_fed = (day 257, slot 0)
```

The guard is the part that must be tested deliberately. Re-publish a time that
jumps forward past a slot already fed:

```
INFO - mqtt: rx feeder/time = 2026-09-14T08:05:00+02:00
INFO - clock: aligned, drift=+5m
INFO - schedule: slot 08:00 already fed today, skipping
```

And a time that jumps past a slot that was **missed** entirely:

```
INFO - schedule: slot 08:00 missed, not catching up
```

Both must skip. A missed meal is preferable to a double one. If either line is
absent and a feed starts instead, stop and fix the guard before flashing any
production unit.

Offline behaviour: stop the broker and confirm the device keeps feeding on the
last schedule it received, with no re-alignment lines. Power-cycle it with the
broker still down and confirm it waits and never guesses a time:

```
WARN - mqtt: disconnected, running on last known schedule
INFO - clock: no time received, waiting
```

### Pause

Four behaviours, and the last two are the ones that break.

```sh
mosquitto_pub -r -t 'feeder/<id>/paused' -m 'ON'
```

```
INFO - mqtt: rx feeder/<id>/paused = ON
INFO - schedule: paused
INFO - mqtt: state published
```

The Home Assistant switch must flip only after that state publish, because it is
not optimistic. A switch that springs back means the device never echoed
`paused` in its state payload.

Then step the clock past a slot while still paused:

```
INFO - schedule: slot 08:00 due but paused, marking consumed
```

**Marking consumed is the point.** Resume and confirm the slot does not fire
retroactively:

```
INFO - mqtt: rx feeder/<id>/paused = OFF
INFO - schedule: resumed, next due 19:00
```

If resuming produces `feed: start` instead, slots are being skipped rather than
consumed, and every unpause replays the last missed meal.

A manual feed while paused must still work. This is the check most likely to be
implemented backwards, because treating pause as a global disable feels tidier:

```
INFO - mqtt: rx feeder/<id>/feed = 1
INFO - feed: start, portions=1
```

Finally, power-cycle while paused. The unit must come back paused, from the
retained topic alone, since nothing is stored in flash:

```
INFO - mqtt: rx feeder/<id>/paused = ON
INFO - schedule: paused
```

A unit that comes back running has a retain flag missing on the command, or is
publishing its first state payload before the retained flag has arrived.

Most of this logic is host-testable with `cargo test`. Use the console to verify
the wiring between the pure logic and the tasks, not the logic itself.

## Step 6 — the Zero boards

Two things change and both can silence the console.

The ESP32-C6-Zero has no USB-serial bridge. Its USB-C port is the chip's native
USB, so `esp-println` must use `jtag-serial` rather than the `uart` setting the
dev kit needs. Flip that feature with the board feature, not by hand:

```toml
[features]
default      = ["board-devkit"]
board-devkit = ["esp-println/uart"]
board-zero   = ["esp-println/jtag-serial"]
```

Then flash with `cargo run --no-default-features --features board-zero`.

The port also changes: the Zero appears as an Espressif USB JTAG/serial debug
unit, so `espflash list-ports` finds it without `--list-all-ports`. Update
`ESPFLASH_PORT`.

Confirm each unit before wiring it into a feeder:

```
INFO - Embassy initialized!
INFO - board: zero, id=<id>
```

Run `espflash board-info` on each of the three and record the MAC, since the id
in every MQTT topic derives from it. Three units must produce three different
ids.

## Step 7 — Home Assistant

Nothing new appears on the serial console at this step. Verification is that all
three units log the same `feeder/time` re-alignment within the same second, and
that a single publish to `feeder/all/feed` produces `feed: start` on all three
consoles at once.

That is the requirement the whole design exists for. Check it with three
monitors open, one per unit.
