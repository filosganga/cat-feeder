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

Switch: GPIO with internal pull-up, other contact to GND. Debounce 30 ms in
software (8 rpm → one edge every ~1.9 s, bouncing is trivial to filter).

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

**Minimum spacing between clicks: ~800 ms.** Just after the motor starts, the
hub is sitting right on an edge; a fraction of a turn can bounce the switch and
produce a spurious falling edge at zero rotation. An edge arriving less than
800 ms after the previous one, or after the motor started, is discarded. At
8 rpm a quarter turn needs ~1900 ms, so this cannot reject a real click, and it
makes counting immune to both bounce and the initial level. It works together
with the 30 ms debounce, not instead of it.

The 5 s no-edge timeout remains the jam guard.

These four cases are exactly why `feeder.rs` takes a `ClickSource` trait:
*starts pressed*, *starts free*, *bounce at t=0*, *no clicks at all* are four
ten-line host tests with a fake source.

### The feeder task owns the motor

One task, one queue. Producers (`mqtt`, `clock`) send portion counts and
nothing else; only this task touches the motor and the switch, so there is no
shared mutable state and no mutex.

```rust
// producers: FEED.try_send(2)
static FEED: Channel<CriticalSectionRawMutex, u8, 8> = Channel::new();

let mut pending: u8 = 0;
loop {
    if pending == 0 {
        motor.brake();
        pending = FEED.receive().await;   // sleep until there is work
        motor.run_forward();
        align(&mut clicks).await;         // only if the switch is free
    }
    // absorb anything that arrived meanwhile, without blocking
    while let Ok(n) = FEED.try_receive() {
        pending = pending.saturating_add(n).min(MAX_PORTIONS);
    }
    match with_timeout(JAM_TIMEOUT, clicks.next()).await {
        Ok(()) => pending -= 1,
        Err(_) => { motor.brake(); pending = 0; report_jammed(); }
    }
}
```

- **Do not stop the motor between portions.** The loop body is one portion,
  but the brake happens only when the queue is empty. Two portions are one
  continuous 180° turn, not two starts.
- **Accumulation falls out of it.** `feed 2` from HA plus `feed 1` from the
  scheduler is three clicks without the motor ever stopping.
- **A jam discards whatever is pending.** Resuming a queue into a jammed
  mechanism is worse than dropping a meal.
- Producers use `try_send` and log the discard, so a full queue never blocks
  the MQTT or clock task.

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
- **Never double-feed.** Track `last_fed: (day, slot)` in RAM; a `time` jump
  forward must not replay a slot already fed nor catch up a missed one. A
  missed meal is preferable to a double one.

## MQTT contract

Broker: Mosquitto (HA add-on / Docker), port 1883, user/pass. Dev broker runs
in Docker on the Mac; production on the Raspberry Pi 5.

```
feeder/<id>/availability   online | offline        (retained, LWT = offline)
feeder/<id>/feed           <portions:u8>           cmd, manual feed
feeder/all/feed            <portions:u8>           cmd, all units at once
feeder/<id>/paused         ON | OFF                cmd, retained, pause the schedule
feeder/schedule            [{"time":"08:00","portions":2}, ...]   retained, from HA
feeder/time                "2026-09-14T08:00:00+02:00"           retained, from HA, every minute
feeder/<id>/state          {"feeding":bool,"jammed":bool,"paused":bool,"last_fed":"..."}
```

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
- `cargo run` = build + `espflash` + serial monitor.
- `espflash board-info` verifies the board/cable.
- Tests of pure logic (schedule evaluation, debounce state machine, double-feed
  guard) live in a `#![cfg_attr(not(test), no_std)]`-style core crate or
  behind `#[cfg(test)]` so they run on the host with `cargo test`.

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
  switch.rs       debounced click stream (async), 30 ms + 800 ms min spacing
  feeder.rs       owns motor + switch; FEED queue, align, count, brake, timeout
  schedule.rs     pure logic: Schedule, LocalClock, next_due(), double-feed guard
  mqtt.rs         connection, LWT, discovery, subscriptions, state publishing
  config.rs       Config + load_config()
build.rs          injects cfg.toml/.env values as env vars
```

Embassy tasks: `net` (Wi-Fi + stack), `mqtt`, `feeder` (owns motor + switch),
`clock` (ticks + re-align). Communicate via `embassy_sync` channels/signals,
not shared mutable statics.

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
2. Switch task: count clicks on the serial console (turn hub by hand)
3. `Motor` + `feed(n)` with the RGB LED as fake motor, then with the DRV8833
4. Wi-Fi + MQTT: ✅ connect, LWT, availability + mocked state. Still to do:
   discovery, subscriptions, manual `feed` command
5. `schedule` + `time` handling, local clock, double-feed guard
6. Board feature for the Zero, flash the three production units
7. Home Assistant automation publishing time + schedule; retire the old PCBs

Later (not now): physical feed button on a spare GPIO (so a manual feed works
with the broker down), runtime Wi-Fi/broker provisioning, battery backup,
buzzer.
