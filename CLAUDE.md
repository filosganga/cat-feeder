# cat-feeder — ESP32-C6 firmware (Rust, no_std, Embassy)

Replacement electronics for three commercial automatic cat feeders. The
original PCB (LCD + RTC + buttons) is removed; the mechanics (5 V DC geared
motor + microswitch on the output hub) are kept. Three units must feed at the
same instant, coordinated by Home Assistant over MQTT.

## Hardware (per unit)

| Part | Notes |
|---|---|
| Waveshare ESP32-C6-DEV-KIT-N8-M | **dev board only** (breadboard, pin headers). WROOM-1 module, 8 MB flash |
| Waveshare ESP32-C6-Zero ×3 | **production boards**, one per feeder. Bare C6FH8, 8 MB flash |
| DRV8833 breakout (black 10-pin) | H-bridge. `nSLEEP`/`ULT` **must be driven high** or the motor won't run |
| Motor DRF-W500CA, 5 V, 8 rpm | geared reducer → stops dead on brake, no coasting past a detent; ~1.9 s per 90°. Back-drivable by hand, but stiff enough that turning the hub is a poor way to test anything |
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
driving**. A bench button pressed twice quickly produces edges far closer
together and every one of them is real. If the rule lived in `switch.rs`, the
stream would silently swallow them and lie about what it observed, and every
bench test would look like a broken debounce.

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
- Wi-Fi + MQTT credentials come from **build-time config** (`cfg.toml`,
  git-ignored, loaded via `build.rs` → `env!()`), read through a `Config`
  struct / `load_config()`. That seam is being used now: see *Provisioning*
  below, which replaces the source of those values without changing anything
  that consumes them. `ap_secret` stays build-time either way.
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

## Provisioning

Credentials come from flash, and a unit with none raises its own Wi-Fi network
and serves a form. **Partly built** — see roadmap step 9 for what runs today.

```text
  boot ── read the record from the nvs partition
           ├── valid   → station mode, connect, run normally
           └── missing → access point, serve the form, save, reboot

  reset button (GPIO10) held → erase the record, reboot   (lands in "missing")
```

**One way in, not two.** The button erases rather than signalling, so "no valid
record" is the only state the boot path has to recognise. There is deliberately
no fall back to setup mode after failing to connect: a router rebooting for five
minutes must not drop a working feeder into setup and stop it feeding.

The value is in *re*-provisioning, not first boot. Flashing a new unit over USB
makes build-time config free; what costs is the Wi-Fi password changing across
three units already screwed into place. That is why the trigger is a button on
the outside of the case rather than a flash-empty check alone.

### The button is not the hub switch

GPIO11 is the rotor microswitch, inside the mechanism and unreachable once
assembled. The reset button is a separate part on **GPIO10** — the other header
pin with no alternate function — and goes somewhere you can press it.

### The setup network

| | |
|---|---|
| SSID | `cat-feeder-<id>`, so three feeders are told apart on a phone |
| Password | `base32(sha256("<ap_secret>:<id>"))`, 60 bits as `XXXX-XXXX-XXXX` |
| Auth | WPA2. WPA3 is available and would add forward secrecy, unverified here |
| Address | `192.168.4.1`, typed in by hand — no captive-portal DNS hijack |

**The salt is the point.** Deriving the password from the MAC alone would not be
a secret: the MAC is in the SSID, it is the BSSID in every beacon frame, and the
derivation is public. WPA2-PSK gives no protection against someone who knows the
passphrase — they capture the handshake and read the session, and that session
is the one where the home Wi-Fi password is typed into the form. `ap_secret` in
`cfg.toml` is what stops that, and it is the only build-time secret this feature
keeps.

Crockford's base32 drops `I`, `L`, `O` and `U`, so nothing on a sticker can be
misread and no word appears by accident. `./dev/ap-password.sh <id>` prints it
so stickers can be made before a unit is first powered on; the firmware prints
it on the console in setup mode as well. **Both must agree byte-for-byte**,
which is why the derivation is plain SHA-256 over `<secret>:<id>` and nothing
more inventive, and why `provisioning::tests::the_password_is_stable` pins
values produced by a separate implementation rather than by the firmware.

### Flash

The `nvs` partition — 24 KB at 0x9000 in the default table — is unused: esp-radio
has a `NVS` symbol but it is a 15-word RAM array in its ESP-IDF shim, not the
partition. No custom partition table is needed, and `espflash` rewrites only the
app partition, so configuration survives a reflash.

This does **not** contradict *no flash persistence* above. That rule is about
schedule and time state, which stay the broker's job. Credentials are the one
thing the broker cannot tell a unit, because they are how it reaches the broker.

A record carries a magic and a CRC-32 so that erased flash (`0xFF` everywhere)
and an interrupted write both read as *unconfigured* rather than as garbage
credentials. A unit that believes a corrupt record sits trying to join a network
that does not exist, and the only way back is the button.

### Still to build — the plan

Paused deliberately, not abandoned. The back half works: a record round-trips
through flash and the boot path reads it. What is missing is everything that
serves the form. Written out here because the API facts below cost an hour to
establish and should not be rediscovered.

**A new gated module, `setup.rs`, entered from the boot path instead of
`seed_config` when there is no record.** It never returns — it reboots once a
record is saved, so the normal path always starts from a clean boot.

1. **Raise the access point.** Build `AccessPointConfig` with
   `ap_ssid(id)`, `ap_password(AP_SECRET, id)` and `Wpa2Personal`, then
   `esp_radio::wifi::new(wifi, ControllerConfig::default()
   .with_initial_config(WifiConfig::AccessPoint(..)))`. There is **no separate
   start call**: `set_config` calls `esp_wifi_start()` whenever the mode
   changes, so applying the initial config brings the network up. Keep the
   controller alive for as long as setup mode runs.
2. **Bring up a second stack** on `interfaces.access_point`, which is an
   ordinary embassy-net `Interface`. `Config::ipv4_static(StaticConfigV4 {
   address: 192.168.4.1/24, gateway: None, dns_servers: empty })`, its own
   `StackResources`, and the existing `net_task` to run it.
3. **Serve DHCP**, or a phone joins and gets nothing. `edge-dhcp` is a codec,
   not a server: `Server::handle_request` takes a parsed `Packet` and returns
   one to send, and the packets are moved by an embassy-net `UdpSocket` bound
   to port 67. Hand out a small pool from 192.168.4.2 upward.
4. **Serve the form** on TCP 80. `provisioning::parse_head` reads the request
   line and `Content-Length`; keep reading until the body is that long.
   - `GET /` (and anything else) → the page.
   - `POST /save` → `provisioning::record_from_form`. On `Ok`, `store.save`,
     answer with a "saved, restarting" page, wait for it to flush, then
     `esp_hal::system::software_reset()`. On `Err`, re-render the page with the
     message and the fields still filled in — a `FormError` naming the field is
     there for exactly this.

**The form's field names must match `record_from_form`:** `wifi_ssid`,
`wifi_password`, `mqtt_host`, `mqtt_port`, `mqtt_user`, `mqtt_password`.

**`mqtt_host` is IP-only.** `mqtt.rs` parses it with `Ipv4Addr::from_str` and
there is no resolver, so the form must reject a hostname with a clear message
rather than accepting one that can never connect. `HOST_LEN` is 64 to leave
room for DNS later.

**No timeout.** A unit in setup mode stays there until someone configures it.
Rebooting out of it would only return to setup mode, and a unit that gives up
while you are fetching your phone is worse than one that waits.

Then the reset button: GPIO10, debounced like `switch.rs`, held for a few
seconds so a brush cannot wipe a working feeder; on release, `store.erase()`
and `software_reset()`.

**Verifying this needs a phone.** The console can show the access point
starting, a station associating, and a request arriving, but joining the network
and submitting the form is not something `dev/flash.sh` can do. Expect a round
or two of iteration on the parts only a real client exercises — captive-portal
probes, keep-alive, and browsers that open several connections at once.

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
  same without reflashing. `./dev/soak.sh [hours]` captures overnight and
  `./dev/soak-report.sh` summarises what happened: reboots, panics, scheduled
  feeds, reconnects. Logs land in `soak/`, which is git-ignored. Prefer these over a hand-written `espflash` command:
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
  wiring.rs       the Bus static's types: FeedChannel, FeederStatus, LastFed
  provisioning.rs pure logic: the flash record, setup-network credentials,
                  the setup form and just enough HTTP
  sha256.rs       pure logic: SHA-256, shared with dev/ap-password.sh
  config.rs       Config + load_config()
build.rs          injects cfg.toml/.env values as env vars

homeassistant/packages/cat_feeder.yaml
                  the other half of the system: publishes time and schedule,
                  the pause helper, the feed-all script. Tracked here and used
                  unchanged on the Pi; install per dev/README.md
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
2. ✅ Switch task: debounced clicks on the console, on a bench button
3. `feed(n)`: ✅ state machine host-tested and verified on hardware with a
   logging fake motor (align, 800 ms rejection, counting, jam, accumulation).
   Still to do: the DRV8833, and with it the **4 clicks per revolution**
   contract. That check belongs here rather than in step 2: the hub can be
   back-driven by hand, but the gear reduction makes turning it steadily
   through a revolution awkward enough that the count is not worth trusting.
   The motor does it in one command, at the speed the mechanism actually sees
4. ✅ Wi-Fi + MQTT: connect, LWT, availability, discovery (button + switch +
   binary_sensor), subscriptions, manual and broadcast `feed`, `paused`, and a
   state payload carrying the feeder's real flags
5. ✅ `schedule` + `time` handling, local clock, double-feed guard. Pure logic
   in `schedule.rs` with 32 host tests, and every rule verified on hardware by
   driving `feeder/time` from the broker
6. ✅ Board feature: `board-devkit` (default) / `board-zero`, selecting the pin
   map, the board name and `esp-println`'s interface (`uart` vs `jtag-serial`).
   Both variants build and lint; the dev kit path is verified on hardware.
   Still to do: flash the three production units, and confirm GPIO11 exists on
   the Zero's pad map before wiring one
7. ✅ Home Assistant: automations publishing time (every minute) + schedule,
   the pause helper and a feed-all script, in
   `homeassistant/packages/cat_feeder.yaml`, verified driving a real scheduled
   feed end to end
8. Retire the old PCBs. Per feeder: remove the original LCD/RTC/button board,
   fit the Zero + DRV8833 + 220 µF, connect the motor and the microswitch, take
   5 V from the feeder's original USB port. The last step in the project and
   the only one with no software in it

### A retained `time` is not a trusted one

`feeder/time` is retained, so a unit that subscribes is handed whatever Home
Assistant last published. While Home Assistant is alive that is under a minute
old. **If it stops while Mosquitto keeps running, that message simply stops
being refreshed and can be any age at all**, and nothing in the payload
distinguishes the two cases. Anchoring to a stale one and feeding from it would
work through the whole day's slots at the wrong times.

So the clock separates *having* a time from *trusting* one:

- A **retained** time starts the clock, because a time is worth having in a log
  line, but leaves the schedule holding.
- A **live** time arms the schedule, because it proves somebody is publishing
  now. The baseline pass then runs against an accurate clock rather than a
  stale one.
- Once armed, retained times are **ignored outright**. Every reconnect replays
  one, and applying it would drag the clock back to whatever the broker holds.
- Trust never lapses. A unit that has been told the time keeps free-running if
  Home Assistant disappears, which is the documented offline behaviour.

MQTT supplies the distinction: the subscription leaves `retain_as_published`
off, so the broker clears the retain flag on everything it forwards live and
sets it only on the messages it replays at subscribe time.

On the console:

```
INFO - clock: started, 2026-09-15T21:45:00+02:00 (retained; waiting for a live time)
INFO - clock: live time 2026-09-15T21:46:00+02:00, schedule armed
```

The consequence to know about: a unit that reboots while Home Assistant is down
but the broker is up will **not feed at all** until Home Assistant returns.
That is deliberate, and the same rule as *power-cycled and no broker → wait,
never guess*. It is only visible on the console, so a unit stuck at
`schedule holding` is silent to Home Assistant — though if Home Assistant is
down, it could not have raised the alarm either.

9. Provisioning: credentials from flash, setup over the unit's own access
   point. Independent of steps 3, 6 and 8 — see *Provisioning* above.
   - ✅ the flash record: format, CRC, and every single-bit flip and
     interrupted write rejected (`provisioning.rs`, host-tested)
   - ✅ *parsing* a submitted form: `x-www-form-urlencoded` into a record, and
     enough HTTP to read a request line and its `Content-Length`. Nothing
     serves it yet — see the access point item below
   - ✅ setup network credentials, and `dev/ap-password.sh` to match
   - ✅ SHA-256 (`sha256.rs`), pinned to NIST vectors and padding boundaries
   - ✅ reading and writing the `nvs` partition (`store.rs`, `esp-storage`
     **0.9** not 0.10 — 0.10 requires an esp-hal 1.2 release candidate).
     Verified: found at 0x9000, seeded, and read back across a full reflash
   - ✅ the boot decision, and `Config` borrowing a record instead of `env!()`
   - ⬜ access point + DHCP server (`edge-dhcp` 0.8, added) + the form over TCP.
     Verified against the pinned sources before writing it: `interfaces.
     access_point` is an ordinary embassy-net `Interface`; the AP needs no
     explicit start, because `set_config` calls `esp_wifi_start()` whenever the
     mode changes, so `wifi::new` with an `AccessPoint` config brings it up;
     the stack takes `Config::ipv4_static`; and `esp_hal::system::
     software_reset()` is the reboot after saving.
     **Final verification needs a phone** — joining the network and submitting
     the form is not something the bench scripts can do.
   - ⬜ the reset button on GPIO10
   - ⬜ retire build-time credentials once setup mode works. `cfg.toml` keeps
     `ap_secret` and nothing else, and the Wi-Fi password stops being compiled
     into the binary at all. To delete, together:
     - `seed_config` in `main.rs` (marked TEMPORARY), and the build-time
       fallback beside it — with no credentials to fall back on, a missing
       `nvs` partition means the unit cannot be provisioned either, so that
       becomes a loud error rather than a quiet default
     - `load_config`, `Config::to_record` and `parse_u16` in `config.rs`
       (`parse_u16` exists only for the port)
     - the key loop, port parsing and CI placeholders in `build.rs`
     - the six credential lines in `cfg.toml` and `cfg.toml.example`

     `Config` stays; it is what the rest of the firmware consumes. Only its
     source changes, which is what `load_config()` was a seam for.

Steps 3, 6 and 8 wait on hardware rather than on code:

| Blocked step | Waiting for |
|---|---|
| 3, the DRV8833 and the clicks-per-revolution contract | the part |
| 6, flashing the three Zeros | the boards |
| 8, retiring the PCBs | 3 and 6 |

Later (not now): a short press on the GPIO10 button feeding one portion, so a
manual feed works with the broker down; battery backup; buzzer.

Also later: a configured feeder timezone (`Europe/Rome`) so the unit can apply
the offset itself and work out DST, instead of assuming it shares a timezone
with the broker. Worth doing only if the broker ever publishes UTC, or moves to
a different zone from the feeders. It means carrying timezone rules on the
device, which is precisely the weight the current design avoids, so it is a
deliberate trade rather than an obvious improvement. The offset is already
parsed and kept in `Wall::offset_minutes`, so the input is there when needed.
