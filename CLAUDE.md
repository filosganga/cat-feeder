# cat-feeder — ESP32-C6 firmware (Rust, no_std, Embassy)

Replacement electronics for three commercial automatic cat feeders. The
original PCB (LCD + RTC + buttons) is removed; the mechanics (5 V DC worm-gear
motor + microswitch on the output hub) are kept. Three units must feed at the
same instant, coordinated by Home Assistant over MQTT.

## Hardware (per unit)

| Part | Notes |
|---|---|
| Waveshare ESP32-C6-DEV-KIT-N8-M | **dev board only** (breadboard, pin headers). WROOM-1 module, 8 MB flash |
| Waveshare ESP32-C6-Zero ×3 | **production boards**, one per feeder. Bare C6FH8, 8 MB flash |
| DRV8833 breakout (black 10-pin) | H-bridge. `nSLEEP`/`ULT` **must be driven high** or the motor won't run |
| Motor DRF-W500CA, 5 V, 8 rpm | worm gear → self-locking, no coasting; ~1.9 s per 90° |
| Microswitch on output hub | **4 clicks per revolution, 1 click = 1 portion** |
| 220 µF 16 V electrolytic | across 5 V/GND next to the DRV8833 (brown-out on motor start) |
| 5 V from the feeder's original USB port | ≥1 A adapter. **No batteries in v1** |

Both boards are the same chip; only GPIO numbers differ. Keep the pin map in
one place (`src/board.rs`) selected by a Cargo feature: `board-devkit`
(default) / `board-zero`.

Avoid strapping pins (GPIO 8, 9, 15) for I/O. GPIO 8 is the on-board RGB LED
on both boards — use it as a "feeding" indicator.

### Motor control (DRV8833)

| IN1 | IN2 | |
|---|---|---|
| 1 | 0 | forward (feed) |
| 0 | 0 | coast |
| 1 | 1 | brake |

Feeding = run forward until N falling edges on the switch, then brake. Stop
**on** the edge, so the hub always parks in the same position. Safety
timeout: if no click within 5 s while running → stop, report `jammed`.

Switch: **GPIO11**, internal pull-up enabled in software
(`InputConfig::default().with_pull(Pull::Up)`), other contact to GND. No
external resistor. Idle reads high, pressed reads low, so a press is a
**falling** edge. Debounce 30 ms in software (8 rpm → one edge every ~1.9 s,
bouncing is trivial to filter).

GPIO11 is on the DEV-KIT's J1 header, third pin in from 5V
(`5V · GPIO3 · GPIO2 · GPIO11`). It and GPIO10 are the only header pins with no
alternate function at all, which is why the switch gets one of them.

⚠️ The nearest ground, J1 pin 15, sits **directly beside 5V**. A ground jumper
off by one position puts 5 V through the switch onto GPIO11 and destroys the
pin. Either double-check that jumper or take a ground from the J3 header, which
has no 5 V neighbour.

### Edges, never levels

Because feeding always brakes **on** a falling edge, the hub normally comes to
rest with the switch already pressed. That is not an invariant — someone can
turn the hub by hand, and the very first run after assembly starts anywhere —
so the state machine is written so the starting level does not matter.

`feed(n)` is three phases:

1. **Align.** If the switch is *not* pressed, run forward without counting
   until the first falling edge. The hub is now in a known position, one
   detent just latched. If it *is* already pressed, do nothing.
2. **Count.** From there, every falling edge is one portion: await `n` of
   them. Starting pressed is irrelevant, because reaching the next falling
   edge requires a release first, and one release→press cycle is exactly a
   quarter turn.
3. **Brake on the nth falling edge.**

Without the align phase, a run that starts with the switch free would make the
first portion short of a full 90°.

**Minimum spacing between clicks: ~800 ms, and it lives in `feeder.rs`, not in
`switch.rs`.** Just after the motor starts, the hub is sitting right on an edge;
a fraction of a turn can bounce the switch and produce a spurious falling edge
at zero rotation. So inside the counting loop, an edge arriving less than 800 ms
after the previous one, or after the motor started, is discarded. At 8 rpm a
quarter turn needs ~1900 ms, so this cannot reject a real click. It works
together with the 30 ms debounce, not instead of it.

The placement matters. That 1900 ms floor only holds **while the motor is
driving**. A hub turned by hand, or a bench button pressed twice quickly, can
legitimately produce edges far closer together. If the rule lived in
`switch.rs`, the stream would silently swallow real edges and lie about what it
observed, and every bench test would look like a broken debounce.

So: `switch.rs` debounces at 30 ms and reports **every** real edge.
`feeder.rs` applies the 800 ms rejection, where the motor-driven assumption
actually holds.

The 5 s no-edge timeout remains the jam guard.

These four cases — *starts pressed*, *starts free*, *bounce at t=0*, *no clicks
at all* — are host tests in `feeder.rs`, along with the one that is easiest to
get wrong: repeated bounce must not postpone jam detection.

Note the 800 ms threshold is a wide margin, not a check that a full quarter
turn happened. Real contact chatter lasts milliseconds; a real detent takes
~1900 ms. Anything in between cannot occur while the motor drives, so the
threshold sits comfortably in the empty middle rather than close to either
edge.

### The feeder task owns the motor

One task, one queue. Producers (`mqtt`, `schedule`) send portion counts and
nothing else; only this task touches the motor and the switch, so there is no
shared mutable state and no mutex.

**The decisions live in a pure state machine, `feeder::Feeder`, not in the
task.** Time arrives as milliseconds in each call, so the machine needs no
clock and no executor and is fully host-tested. The task asks what to do, does
it, and reports back. It decides nothing.

```rust
// producers: FEED.try_send(2)
static FEED: Channel<CriticalSectionRawMutex, u8, 8> = Channel::new();

let mut feeder = Feeder::new();
loop {
    match feeder.action(now_ms()) {
        Action::Idle => {
            motor.brake();
            feeder.request(FEED.receive().await);        // sleep until there is work
            feeder.start(now_ms(), switch.is_pressed()); // level decides alignment
            motor.run_forward();
        }
        Action::Turning { jam_timeout_ms } => {
            // a request arriving mid-turn has to *wake* this loop
            match select3(clicks.next_click(),
                          FEED.receive(),
                          Timer::after_millis(jam_timeout_ms)).await {
                Either3::First(_)  => { feeder.on_click(now_ms()); }
                Either3::Second(n) => { feeder.request(n); }   // no start, no motor
                Either3::Third(_)  => { motor.brake(); feeder.on_timeout(); }
            }
        }
    }
}
```

- **Do not stop the motor between portions.** `action` keeps reporting
  `Turning` until the last click, so two portions are one continuous 180° turn
  rather than two starts.
- **Accumulation falls out of it.** `feed 2` from HA plus `feed 1` from the
  scheduler is three clicks without the motor ever stopping.
- **`FEED` belongs in the `select`, not drained before it.** Draining with
  `try_receive` at the top of the loop looks equivalent and is not: the loop
  then blocks for the whole jam budget, so a request arriving mid-turn is not
  seen until the next click — by which time the portion has finished, the
  machine has gone idle and the motor has braked. Observed on hardware: three
  `feed 1` within 170 ms produced one portion, not three. This is the mistake
  the rule above exists to prevent, and it is invisible in the log unless the
  `pending=` lines are read against the timestamps.
- **`jam_timeout_ms` is the remaining budget**, measured from the last counted
  click, never a fresh 5 s. Otherwise sustained bounce would postpone jam
  detection indefinitely and leave the motor energised against a stuck hub.
- **A jam discards whatever is pending.** Resuming a queue into a jammed
  mechanism is worse than dropping a meal. The jam flag clears by itself when
  a portion is next counted.
- Producers use `try_send` and log the discard, so a full queue never blocks
  the MQTT or clock task.
- Reading the switch level is I/O, so it is an input to `start` rather than
  something the machine works out. That is the only place the task supplies a
  fact rather than an event.

## Architecture — decided, do not re-litigate

- **No local RTC, no NTP, no flash persistence.** The broker is a bulletin
  board: Home Assistant publishes (retained) the current time and the feeding
  schedule; each ESP keeps them in RAM and advances a local tick counter,
  re-aligned on every `time` message. Offline → keep running on the last
  received schedule/time. Power-cycled and no broker → wait, never guess.
- **No batteries, no sleep modes, no USB detection** in v1.
- Wi-Fi + MQTT credentials come from **build-time config** (`cfg.toml` or
  `.env`, git-ignored, loaded via `build.rs` → `env!()`). Read through a
  `Config` struct / `load_config()` so a later runtime-provisioning version
  (captive portal / BLE) is a drop-in.
- Device id = derived from the MAC. One binary flashes all units.
- **Never double-feed.** A missed meal is preferable to a double one. Three
  mechanisms in `schedule.rs`, each covering a failure the others cannot see:
  a **consumed marker** in RAM, `(day, minute-of-day)` of the last slot
  resolved; a **baseline pass**, so the first look at the clock after boot only
  records where the day is and never feeds; and a **lateness limit** of two
  minutes, so a `time` jump forward is never mistaken for a slot falling due.
  The marker is keyed on time of day rather than slot index, because Home
  Assistant can republish a schedule with a slot inserted or removed and an
  index would then point at a different meal.

## MQTT contract

Broker: Mosquitto (HA add-on / Docker), port 1883, user/pass. Dev broker runs
in Docker on the Mac; production on the Raspberry Pi 5.

```
feeder/<id>/availability   online | offline        (retained, LWT = offline)
feeder/<id>/feed           <portions:u8>           cmd, manual feed
feeder/all/feed            <portions:u8>           cmd, all units at once
feeder/<id>/paused         ON | OFF                cmd, retained, pause the schedule
feeder/schedule            [{"time":"08:00","portions":2}, ...]   retained, from HA
feeder/time                2026-09-14T08:00:00+02:00             retained, from HA, every minute
feeder/<id>/state          {"feeding":bool,"jammed":bool,"paused":bool,"last_fed":"..."}
```

Three different limits apply, and they are easy to confuse because two of them
are the same number:

| Constant | Value | Limits |
|---|---|---|
| `MAX_SLOTS` (`schedule.rs`) | 8 | **meals per day** |
| `MAX_PORTIONS` (`portions.rs`) | 10 | portions owed at once, so portions per meal |
| `FEED_DEPTH` (`wiring.rs`) | 8 | unread feed **requests** in the channel |

Two meals a day is the usual case, but three, four or five are ordinary and all
fire. A schedule with more than `MAX_SLOTS` entries is rejected whole rather
than truncated, because a silently shortened one drops meals with nothing to
show for it, and the unit keeps running the schedule it already had.

`MAX_PORTIONS` caps a single meal, not the day: eight meals of ten portions is
80 portions, because the queue drains between them. `FEED_DEPTH` counts
messages rather than portions — one `feed 3` occupies one of the eight — and
only matters when producers outrun the feeder task.

`feeder/time` is a bare ISO 8601 string, which is what `{{ now().isoformat() }}`
publishes. Wrapping double quotes and fractional seconds are tolerated too, so
a hand-published JSON string also works.

The **offset is recorded but never applied**, and that is a decision rather than
an oversight. Home Assistant publishes its own local time and the feeders live
in the same house, so the wall-clock fields already arrive in the frame the
schedule is written in: `08:00` in a slot means 08:00 on the kitchen wall.
There is nothing to convert to, and no timezone rules are needed on the device.
Daylight saving then costs nothing — in October Home Assistant simply starts
sending `+01:00` and the wall-clock fields shift with it.

The assumption this rests on is **the broker and the feeders share a
timezone**. The one realistic way to break it is publishing `utcnow()` instead
of `now()`, which would still look like a valid time while moving every meal by
the offset. That is why the offset is kept and printed at startup
(`clock: started, 2026-09-15T09:00:00+02:00`) rather than dropped: it turns a
silent hour-long error into the first line on the console.

`last_fed` is reported the same way, local with the published offset, and
covers scheduled feeds only — a manual feed reaches the feeder task, which has
no clock, and Home Assistant already records button presses in its own history.

Home Assistant MQTT discovery: on connect, publish **retained** config to
`homeassistant/<component>/feeder_<id>/<object>/config` for: a `button`
(feed), a `switch` (paused) and a `binary_sensor` (jammed). All share the same
`device` block so HA groups them into one device. Then publish `online`.

**Manual feeds accumulate.** The button always sends `1`. Three presses in a
row mean three portions, even if they land while the motor is already running:
`mqtt` forwards the count into the `FEED` queue and the feeder task absorbs it
into `pending` without stopping, as described in *The feeder task owns the
motor*. `MAX_PORTIONS` is 10, clamped with a warning, so a stuck automation
cannot empty the hopper. There is no "default portion size" — every feed path
states its own count, the button as `1` and each schedule slot as its own
`portions`.

Accumulation applies to manual feeds only. Scheduled feeds pass the
`last_fed` guard *before* being added to the counter, so the never-double-feed
rule is unaffected.

**Pause stops the schedule, not the feeder.** `feeder/<id>/paused` is retained,
per unit, and there is deliberately no `feeder/all/paused`: two retained topics
setting the same flag would race on reconnect. HA pauses all three by publishing
to each unit's topic.

- Manual `feed` still works while paused. Pause is for the schedule only, so
  you can always top up a bowl by hand.
- Slots that fall due while paused are **marked consumed, not fed**. Resuming
  never replays a slot and never catches one up, same rule as a `time` jump.
- Paused is not offline. The unit stays `online` and keeps re-aligning its
  clock, so resuming is instant.
- Retained because there is no flash: a unit that reboots while paused must
  come back paused.

A feeder left paused is the one failure mode where cats do not eat and nothing
alarms. Keep `paused` visible in the state payload and as a switch in HA.

## Toolchain

- Rust stable + `riscv32imac-unknown-none-elf` (RISC-V — no espup needed).
- Generated with `esp-generate --chip esp32c6` with: `unstable-hal`, `alloc`,
  `wifi` (esp-radio), `embassy`, `log` + `esp-println`, `esp-backtrace`,
  board `esp32c6-wroom-1`. **No BLE, no probe-rs/defmt.**
- `./dev/flash.sh [secs] [filter]` = build + flash + bounded capture, with each
  line annotated by the gap since the previous one. `./dev/capture.sh` does the
  same without reflashing. Prefer these over a hand-written `espflash` command:
  they pin the right port and avoid `--no-reset`, which halts the application
  so only the bootloader prints.
- `cargo run` = build + `espflash` + interactive monitor, for driving by hand.
- `espflash board-info` verifies the board/cable.
- Tests of pure logic (portion accounting, schedule evaluation, double-feed
  guard) live behind `#[cfg(test)]` in modules that do **not** touch esp-hal,
  and run on the host:

  ```sh
  cargo test --lib --target "$(rustc -vV | awk '/^host:/{print $2}')"
  ```

  The explicit target is not optional: `.cargo/config.toml` points cargo at the
  board, so plain `cargo test` builds the tests for the ESP32-C6 and fails to
  link. CI runs the same command against `x86_64-unknown-linux-gnu`.

  Two things keep the host build working, and both are easy to break:
  - `src/lib.rs` gates every hardware module on `#[cfg(target_os = "none")]`.
  - `Cargo.toml` puts every esp-* / embassy-* dependency under
    `[target.'cfg(target_os = "none")'.dependencies]`. Gating the modules alone
    is not enough, because cargo still compiles the dependencies.

  `build.rs` likewise only emits `-Tlinkall.x` and the linker error-handling
  hook for the bare-metal target; the host linker rejects both. It also falls
  back to placeholder credentials when `cfg.toml` is absent **and** `CI` is
  set, so CI can build without secrets while a local build still fails loudly.

  New pure logic goes in a module listed above the gate in `lib.rs`. If it
  needs a peripheral, it is not pure logic.

Check the exact esp-hal / esp-radio / embassy versions in `Cargo.toml` and
follow the matching `examples/` in the esp-hal repo; the API (e.g.
`Input::new(gpio, InputConfig::default().with_pull(Pull::Up))`) shifts between
releases. Do not guess from memory — read the pinned version's docs.

## Local dev stack

Mosquitto + Home Assistant in Docker on the Mac, so firmware work needs no
Raspberry Pi. Details and troubleshooting in `dev/README.md`.

```sh
./dev/bootstrap.sh          # once: broker password file (feeder / feeder-dev)
docker compose up -d        # broker on 1883, HA on http://localhost:8123
./dev/watch.sh              # tail feeder/# and homeassistant/#
docker compose down -v      # stop and wipe every retained message
```

Same broker, three addresses: `localhost` from the Mac, `mosquitto` from the
Home Assistant container, the Mac's LAN address (`ipconfig getifaddr en0`) from
the ESP32. `down -v` is the only way to test a cold boot, since every piece of
persistent state in this design lives in the broker's retained messages.

## Code organisation

```
src/
  main.rs         wiring: peripherals, tasks, executor
  board.rs        pin map per board (feature-gated)
  motor.rs        Motor { run_forward(), brake() } over two Output pins + nSLEEP
  switch.rs       debounced click stream (async), 30 ms; reports every edge
  feeder.rs       owns motor + switch; FEED queue, align, 800 ms spacing,
                  count, brake, jam timeout
  schedule.rs     pure logic: Schedule, LocalClock, next_due(), double-feed guard
  mqtt.rs         connection, LWT, discovery, subscriptions, state publishing
  wiring.rs       the types tasks share: FeedChannel/FeedSender, FeederStatus
  config.rs       Config + load_config()
build.rs          injects cfg.toml/.env values as env vars
```

Embassy tasks: `net` (Wi-Fi + stack), `mqtt`, `switch` (owns the GPIO),
`feeder` (owns the motor), `schedule` (owns the clock, ticks once a second and
re-aligns). They communicate through the one `wiring::Bus` static, which names
every shared handle and documents who writes each one.

## Conventions

- `no_std`, `#![no_main]`; use `heapless` for strings/vecs, `esp-alloc` only
  where esp-radio needs it.
- Logging via `log::{info,warn,error}` — this is the only debug channel.
- Hardware abstractions are traits (`MotorDriver`, `ClickSource`) so the
  feeder logic can be tested on the host with fakes, and so the RGB LED can
  stand in for the motor when no driver is connected.
- Keep changes small and flash-testable; every step should be verifiable on
  the serial console.
- Don't add features the plan doesn't call for (buzzer, display, battery,
  captive portal) without asking.

## Roadmap

1. ✅ Toolchain + blinky on the DEV-KIT
2. ✅ Switch task: debounced clicks on the console (bench button; still to
   re-check against the real hub, where the contract is 4 clicks/revolution)
3. `feed(n)`: ✅ state machine host-tested and verified on hardware with a
   logging fake motor (align, 800 ms rejection, counting, jam, accumulation).
   Still to do: the DRV8833 itself
4. ✅ Wi-Fi + MQTT: connect, LWT, availability, discovery (button + switch +
   binary_sensor), subscriptions, manual and broadcast `feed`, `paused`, and a
   state payload carrying the feeder's real flags. `schedule` and `time` are
   subscribed and logged but not acted on — that is step 5
5. ✅ `schedule` + `time` handling, local clock, double-feed guard. Pure logic
   in `schedule.rs` with 32 host tests, and every rule verified on hardware by
   driving `feeder/time` from the broker
6. Board feature for the Zero, flash the three production units
7. Home Assistant automation publishing time + schedule; retire the old PCBs

Later (not now): physical feed button on a spare GPIO (so a manual feed works
with the broker down), runtime Wi-Fi/broker provisioning, battery backup,
buzzer.

Also later: a configured feeder timezone (`Europe/Rome`) so the unit can apply
the offset itself and work out DST, instead of assuming it shares a timezone
with the broker. Worth doing only if the broker ever publishes UTC, or moves to
a different zone from the feeders. It means carrying timezone rules on the
device, which is precisely the weight the current design avoids, so it is a
deliberate trade rather than an obvious improvement. The offset is already
parsed and kept in `Wall::offset_minutes`, so the input is there when needed.
