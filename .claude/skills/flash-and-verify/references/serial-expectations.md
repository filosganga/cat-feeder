# What the console must show, per roadmap step

- [How to read this file](#how-to-read-this-file)
- [Step 1 — toolchain and blinky](#step-1--toolchain-and-blinky)
- [Step 2 — switch task](#step-2--switch-task)
- [Step 3 — motor and feed(n)](#step-3--motor-and-feedn)
- [Step 4 — Wi-Fi and MQTT](#step-4--wi-fi-and-mqtt)
- [Step 5 — schedule, time and the double-feed guard](#step-5--schedule-time-and-the-double-feed-guard)
- [Step 6 — the Zero boards](#step-6--the-zero-boards)
- [Step 7 — Home Assistant](#step-7--home-assistant)
- [Step 8 — retiring the old PCBs](#step-8--retiring-the-old-pcbs)
- [Step 9 — provisioning](#step-9--provisioning)

## How to read this file

Each step lists the log lines that step must emit, the physical action that
triggers them, and the failure signatures worth recognising. The exact wording
is a proposal: implement it as written so this file stays usable as an
acceptance test, or change both the code and this file together.

Steps marked *observed* carry real transcripts. The rest are contracts, not
recordings: when you first reach one, replace its expected block with what the
console actually printed. Steps 3 (the motor itself), 6 (flashing the Zeros),
8 and the access point half of 9 are the ones still unobserved, all of them
waiting on hardware or a phone.

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

### The real hub belongs to step 3, not here

The four-clicks-per-revolution contract is checked in
[step 3](#step-3--motor-and-feedn), with the motor driving.

The hub *can* be back-driven by hand, but the gear reduction makes it hard
enough that turning it steadily through a full revolution is an awkward and
unconvincing test: it is slow, the speed is uneven, and any hesitation on a
detent invites exactly the bounce the test is supposed to be measuring. Driving
it with the motor takes one command, runs at the speed the mechanism actually
sees, and is repeatable.

A bench button is therefore not a poor substitute at this step, it is the whole
of it: it proves the debounce, the pull-up, and that every edge is reported.

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

### Four clicks per revolution

The mechanical contract, and the right place for it. The hub can be back-driven
by hand, but the gear reduction makes turning it steadily through a revolution
awkward enough that the result is not worth trusting. Let the motor do it.

Action: mark the hub, then trigger a **four**-portion feed.

```
INFO - feed: start, portions=4
INFO - feed: click, 3 to go
INFO - feed: click, 2 to go
INFO - feed: click, 1 to go
INFO - feed: done
```

The mark must come back to where it started, one full revolution, in about
7.6 s. Then:

- **Mark short of a full turn** — more than four detents per revolution. Every
  portion is smaller than intended and the whole schedule under-feeds.
- **Mark past the start** — fewer than four, or an edge is being missed.
- **Right place, wrong time** — count the seconds. Four portions much faster
  than 7.6 s means bounce is being counted as detents, and the 800 ms rejection
  is not doing its job.

Repeat it three or four times without stopping. The mark must return to the
same place every revolution, not drift, since a drift of a fraction of a detent
per turn compounds into a missed meal over a day.

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

Status: observed on the dev kit against the Docker broker, with `LogMotor`
standing in for the DRV8833.

Action: flash with credentials configured, and watch the console and the broker
side by side. `./dev/watch.sh` tails the broker.

Observed, from a cold boot:

```
INFO (270) - Embassy initialized!
INFO (274) - board: devkit, id=db0260                                   (+4 ms)
INFO (368) - wifi: connecting to <ssid>                                 (+3 ms)
INFO (375) - switch: watching GPIO11, currently released                (+7 ms)
INFO (1630) - wifi: associated                                          (+4 ms)
INFO (11774) - wifi: connected, ip=192.168.68.123/24                    (+10144 ms)
INFO (11780) - mqtt: connecting to 192.168.68.108:1883                  (+6 ms)
INFO (11960) - mqtt: connected, id=feeder_db0260                        (+6 ms)
INFO (12052) - mqtt: discovery published                                (+92 ms)
INFO (12085) - mqtt: online                                             (+33 ms)
INFO (12161) - mqtt: subscribed                                         (+76 ms)
INFO (12166) - mqtt: paused = OFF                                       (+5 ms)
INFO (12170) - mqtt: feeder/schedule received, 61 bytes, not handled yet (+4 ms)
INFO (12207) - mqtt: feeder/time received, 27 bytes, not handled yet    (+37 ms)
```

The order is the contract: discovery, then `online`, then the subscriptions.
Note the last three lines — every retained message landed within 46 ms of
subscribing, comfortably inside the one-second grace the firmware waits before
publishing its first state. Widen that grace only if those lines start arriving
after it.

Verify on the broker too, not only on the console: three retained configs under
`homeassistant/`, then `online` retained on `feeder/<id>/availability`.

Observed in Home Assistant: one device, `Cat feeder <id>`, firmware `0.1.0`,
with **Feed** and **Paused** under Controls and **Jammed** under Diagnostic.
Three separate entities with no device card means the `device` block or a
`unique_id` is missing; Jammed sitting in Controls means `entity_category` was
dropped. The end-to-end check is pressing Feed in Home Assistant and watching
the console: the command must reach the feeder task, not merely appear on the
broker.

Then publish a manual feed and watch both sides:

```sh
./dev/watch.sh &
docker compose exec -T mosquitto \
  mosquitto_pub -h localhost -u feeder -P feeder-dev -t 'feeder/<id>/feed' -m '2'
```

Observed, with nothing turning the hub, which is the jam path:

```
INFO (20011) - mqtt: feed 2
INFO (20014) - motor: forward                                           (+3 ms)
INFO (20017) - feed: start, portions=2, needs aligning                  (+3 ms)
INFO (25014) - motor: brake                                             (+4997 ms)
WARN (25017) - feed: no click for 5s, jammed; pending discarded         (+3 ms)
```

The broker side shows the state following it, `"feeding":true` then
`"jammed":true`. That is the useful end-to-end check without a motor: it proves
the command reached the queue, the feeder acted on it, and the real flags — not
a mock — reach Home Assistant.

A payload that is not a number is rejected and nothing moves:

```
WARN (28115) - mqtt: feed payload is not a portion count
```

Then the accumulation test, which is the part most likely to be wrong. Send
three presses back to back, faster than one feed cycle:

```sh
for i in 1 2 3; do
  docker compose exec -T mosquitto \
    mosquitto_pub -h localhost -u feeder -P feeder-dev -t 'feeder/<id>/feed' -m '1'
done
```

Observed, with the requests spaced a second apart so the ordering is legible:

```
INFO (18613) - mqtt: feed 1
INFO (18616) - motor: forward                                           (+3 ms)
INFO (18619) - feed: start, portions=1, needs aligning                  (+3 ms)
INFO (19747) - mqtt: feed 1                                             (+1128 ms)
INFO (19751) - feed: pending=2                                          (+4 ms)
INFO (20856) - mqtt: feed 2                                             (+1105 ms)
INFO (20859) - feed: pending=4                                          (+3 ms)
INFO (23617) - motor: brake                                             (+2758 ms)
WARN (23620) - feed: no click for 5s, jammed; pending discarded         (+3 ms)
```

Three things in that transcript are the actual test, and all three are easy to
miss:

- **`motor: forward` appears once.** A second one means the machine went idle
  between portions and restarted, which is the failure the `FEED`-in-the-select
  shape exists to prevent. See *The feeder task owns the motor* in CLAUDE.md.
- **`pending=` rises within ~4 ms of each command.** Tens of milliseconds is
  fine; a delay of a whole portion time means the request waited for a click to
  wake the loop instead of waking it itself. That is the bug, and with no motor
  attached it hides completely — nothing wakes the loop at all, so the extra
  requests are only picked up after the jam and look like fresh feeds.
- **The brake lands 5000 ms after `feed: start`,** not 5000 ms after the last
  command. A mid-turn request must not postpone jam detection.

Count the clicks, not the log lines: the hub must turn **four** detents here in
one continuous run. Two failure modes to watch for, both of which look fine in
the log:

- Only one portion dispensed. The commands arriving during a feed were dropped
  instead of accumulating.
- The task deadlocks after the first feed. The counter is being read under a
  lock the feeder task still holds.

Then check the clamp by asking for 20:

```
WARN - feed: clamped at 10 portions, 10 dropped
```

**Time the commands after the board is up.** A `feed` is deliberately never
retained, so anything published while the unit is still being flashed is simply
gone — the broker shows it, the console does not, and it looks like a dropped
subscription. Boot to `mqtt: subscribed` took 12 s in the capture above, and the
flash before it another 25 s.

Pull the power and confirm the broker shows `offline` on the availability topic:
that proves the last will was registered in the CONNECT packet. Reflashing does
it too — the will fires as the old firmware's socket dies.

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

Status: observed on the dev kit.

**Drive the clock from the broker.** The firmware trusts `feeder/time`
completely and keeps no RTC, so publishing times by hand walks it through a
whole day in seconds. Waiting for real mealtimes to test a schedule is a way to
test it roughly twice a day.

```sh
pub() { docker compose exec -T mosquitto \
  mosquitto_pub -h localhost -u feeder -P feeder-dev "$@"; }

pub -r -t 'feeder/schedule' -m '[{"time":"08:00","portions":1},{"time":"12:00","portions":2}]'
pub -r -t 'feeder/time' -m '2026-09-15T09:00:00+02:00'
```

Then step the clock with further retained publishes to `feeder/time`. The whole
sequence below took 120 seconds. `dev/` in the scratchpad of the session that
first ran it has a script; it is nothing more than the publishes in order.

Observed, with the gaps between lines stripped for readability:

```
INFO - clock: no trusted time yet, schedule holding  # before the broker is up
INFO - mqtt: subscribed
INFO - clock: started, 2026-09-15T09:00:00+02:00
INFO - schedule: 2 slots
INFO - schedule: slot 08:00 already past at startup  # baseline: no feed

INFO - clock: aligned, drift=10784s                  # step to 11:59:55
INFO - schedule: slot 12:00 due, feeding 2           # step to 12:00:03
INFO - motor: forward
INFO - feed: start, portions=2, needs aligning

INFO - clock: aligned, drift=71972s                  # step to the next day
INFO - schedule: slot 08:00 due, feeding 1           # a new day re-arms slots

INFO - mqtt: paused = ON
INFO - schedule: slot 12:00 due but paused, marking consumed
INFO - mqtt: paused = OFF
                                                     # nothing: no replay
INFO - schedule: slot 08:00 missed by 30m, not catching up
```

Five rules, each of which will silently feed the cats twice if it breaks:

- **`slot ... already past at startup`, and no feed.** The guard lives in RAM,
  so a reboot a few seconds after 08:00 would otherwise dispense 08:00 again.
  The first look at the clock only takes a baseline.
- **`missed by 30m, not catching up`.** A clock correction that jumps the unit
  past a slot is not the slot falling due. Anything more than two minutes late
  is marked consumed instead.
- **A new day re-arms every slot,** but only by date, never by replaying.
- **`due but paused, marking consumed`,** then silence on resume. Marking
  consumed is the whole point: if slots were merely skipped, every unpause would
  dispense the meal that was deliberately missed.
- **No `schedule:` line at all** for a slot already resolved, however many times
  the clock is stepped over it.

### The schedule holds until a live time arrives

Against a broker that already has a retained `feeder/time`, the boot sequence
has an extra step that is easy to mistake for a fault:

```
INFO (14578) - clock: started, 2026-09-15T21:45:00+02:00 (retained; waiting for a live time)
INFO (14587) - schedule: 2 slots
INFO (34593) - clock: live time 2026-09-15T21:46:00+02:00, schedule armed   (+20006 ms)
INFO (34600) - schedule: slot 19:00 already past at startup
```

The unit sat for twenty seconds knowing the time and refusing to use it. That
is correct: a retained `feeder/time` is whatever the broker last stored, which
is arbitrarily old if Home Assistant stopped, and the baseline must not run
against a stale clock. Up to a minute of this is normal, since Home Assistant
publishes on the minute.

**A unit that never prints `schedule armed` will never feed on schedule.** If
it is still holding after a couple of minutes, Home Assistant is not publishing
— check that its container is up and the publish-the-time automation is
enabled. `./dev/soak-report.sh` calls this out for exactly that reason.

**Read the offset on that first line.** The firmware does not apply it — slots
are local wall-clock times and the feeders share a house with the broker — so it
is printed precisely because nothing else would notice if it were wrong. An
automation publishing `utcnow()` instead of `now()` still looks like a valid
time while moving every meal by the offset, and `+00:00` on a unit in Rome is
the only visible sign.

Check `feeder/<id>/state` on the broker afterwards. `last_fed` carries the local
time of the last **scheduled** feed with the offset as published, and is the
quickest confirmation the whole path ran:

```
{"feeding":false,"jammed":true,"paused":false,"last_fed":"2026-09-16T08:00:02+02:00"}
```

A manual feed while paused must still work — the check most likely to be built
backwards, because treating pause as a global disable feels tidier:

```sh
pub -r -t 'feeder/<id>/paused' -m 'ON'
pub -t 'feeder/<id>/feed' -m '1'
```

```
INFO - mqtt: feed 1
INFO - feed: start, portions=1
```

Offline behaviour: stop the broker and confirm the unit keeps feeding on the
last schedule it received, with no `clock: aligned` lines, since the local clock
free-runs. Power-cycle with the broker still down and confirm it waits rather
than guessing a time:

```
INFO - clock: no time received, waiting
```

Finally, power-cycle while paused. The unit must come back paused from the
retained topic alone, since nothing is stored in flash. One that comes back
running has a retain flag missing on the command, or is publishing its first
state payload before the retained flag arrives.

Most of this logic is host-testable and 32 tests cover it. Use the console to
verify the wiring between the pure logic and the tasks, not the logic itself.

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

Status: observed on one unit. The three-unit check below still needs the Zeros.

Home Assistant's half lives in `homeassistant/packages/cat_feeder.yaml`;
`dev/README.md` covers installing it. Once it is running, the console shows the
schedule arriving from Home Assistant rather than from a hand publish, and slots
firing at their real times:

```
INFO - clock: started, 2026-09-15T19:56:00+02:00
INFO - schedule: 2 slots
INFO - schedule: slot 19:56 due, feeding 3
```

Confirm on the broker that `last_fed` matches the slot, which is the proof the
whole path ran rather than just the publish:

```
"last_fed":"2026-09-15T19:56:00+02:00"
```

**Check the offset on that `clock: started` line against where you live.** Home
Assistant's container defaults to UTC, and `TZ` in `compose.yaml` is what makes
it local. Get it wrong and every meal lands an hour or two out while every
entity still looks healthy — this line is the only place it shows.

Then the requirement the whole design exists for, once all three units are
built: a single publish to `feeder/all/feed`, or `script.cat_feeder_feed_all`,
must produce `feed: start` on all three consoles at once, and all three must log
the same `feeder/time` re-alignment within the same second. Check it with three
monitors open, one per unit.

## Step 8 — retiring the old PCBs

No serial output belongs to this step; it is screwdriver work. The console check
is simply that a feeder still behaves after being reassembled, so run step 3's
`feed(n)` checks again on the real mechanism once each unit is in its case —
especially the four-clicks-per-revolution contract, which is the thing most
likely to differ between a bench hub and an assembled one.

Watch the boot banner's `rst:` line on the first feed after assembly. A reset
the moment the motor starts is the 220 µF capacitor, not the firmware. See
[troubleshooting.md](troubleshooting.md).

## Step 9 — provisioning

Partly built. What runs today is the flash record and the boot decision:

```
INFO (285) - store: nvs at 0x9000, 24576 bytes
INFO (289) - store: no record yet
INFO (293) - setup: would raise cat-feeder-db0260 / DAKS-2W9X-NVQG
INFO (342) - store: seeded from cfg.toml
```

and on the next boot, including after a full reflash:

```
INFO (290) - store: configured for fdlgrm via 192.168.68.108:1883
```

**That second line is the test.** Configuration lives in the `nvs` partition and
`espflash` rewrites only the app partition, so a unit keeps its credentials
across every `cargo run`. If it says `no record yet` twice in a row, the write
is failing — check `store: nvs at ...` reports a partition at all.

`setup: would raise ...` is the temporary stand-in for setup mode. Cross-check
that password against `./dev/ap-password.sh`, which derives it independently:
they must match exactly, or the sticker on the unit is wrong.

`store: seeded from cfg.toml` is also temporary, and disappears when setup mode
lands — see roadmap step 9.

### When the access point is built

Not written yet. What it must show:

```
INFO - store: no record yet
INFO - setup: access point cat-feeder-db0260 up, browse to 192.168.4.1
INFO - setup: station connected
INFO - setup: GET /
INFO - setup: POST /save, saving
INFO - store: saved
INFO - setup: restarting
```

then a clean boot straight into `store: configured for ...`.

**This one cannot be verified from the bench alone.** Joining the network and
submitting the form needs a phone in someone's hand; the console only shows the
device's half. Watch particularly for what a real browser does and a test client
does not: captive-portal probe requests to odd paths, several connections at
once, and connections opened and dropped without a request.
