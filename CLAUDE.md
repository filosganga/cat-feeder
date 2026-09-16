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
| Motor DRF-W500CA, 5 V, 8 rpm | geared reducer → stops dead on brake, no coasting past a detent; ~1.9 s between detents. Back-drivable by hand, but stiff enough that turning the hub is a poor way to test anything |
| Microswitch on output hub | **1 click = 1 portion.** That is the entire contract |
| 220 µF 16 V electrolytic | across 5 V/GND next to the DRV8833 (brown-out on motor start) |
| 5 V from the feeder's original USB port | ≥1 A adapter. **No batteries in v1** |

### The third feeder is a different brand

Two of the three units are the same model. The third is a different brand,
similar-looking but **not yet opened**, and the mechanical figures in the table
above were measured on the matching pair only.

Three things matter, and only one of them is a number:

| Figure | What breaks if it differs |
|---|---|
| 1 click = 1 portion | every `portions` count, from the HA button to each schedule slot |
| the detent interval (~1.9 s here) | the minimum click spacing and the jam timeout are both derived from it — see *Per-unit mechanical timing* |
| a microswitch on the output hub at all | `switch.rs` assumes a pull-up and a falling edge — an optical or hall sensor is a different shape entirely |

**Clicks per revolution is not on that list**, though it used to be. Nothing in
the firmware counts revolutions; it was only ever a way to work out the detent
interval from the motor's rpm. Measure the interval directly and the revolution
count tells you nothing more. It keeps one small use as a bench check — a full
turn should give a *stable* count, whatever that count is, which catches clicks
being missed or doubled — but that is a check, not a contract.

So the third unit needs the detent interval measured, and the switch confirmed
to be a switch. See *Per-unit mechanical timing* for where the number goes.

**Measure what one click actually dispenses** while it is open — by weight, or
by counting clicks into a measuring spoon — and compare it with the other two.
`feeder/schedule` is one retained topic shared by all three, so `portions: 2`
reaches every unit identically, and a mechanism that dispenses a different
amount per click needs a per-unit scale. That is built: see *Per-unit portion
size*. What is needed from the bench is the ratio.

Both boards are the same chip; only GPIO numbers differ. Keep the pin map in
one place (`src/board.rs`) selected by a Cargo feature: `board-devkit`
(default) / `board-zero`.

### Which pins are usable

**The Zero is the binding constraint, and it is tighter than the dev kit.** Its
pad map brings out GP0–GP9 and GP12–GP23, with GP16/GP17 appearing as `TX`/`RX`.
**GPIO10 and GPIO11 are not brought out at all**, on neither the edge
castellations nor the back pad row. Both were in the original pin map and both
have moved; see `src/board.rs`, which is still the one place any number lives.

Then subtract what is already spoken for:

| Pin | Why not |
|---|---|
| GPIO4, GPIO5, GPIO8, GPIO9, GPIO15 | strapping, sampled at reset |
| GPIO12, GPIO13 | native USB D−/D+; on the Zero, the only console there is |
| GPIO8 | also the onboard WS2812, so already committed |

That leaves GP0–GP3, GP14 and GP18–GP22 on the edge, plus GP6, GP7 and GP23 on
the back pads — thirteen usable against seven needed, so the display and the
LED both fit with room left.

Note the strapping list is five pins, not the three this file used to name:
GPIO4 and GPIO5 are strapping on the C6 as well.

"No alternate function" was the rule that first picked GPIO10 and GPIO11. It
does not really apply here: on the C6 peripheral signals route through a GPIO
matrix, so the labels on a pinout diagram are a convention rather than a
restriction, and any pin outside that table will do.

GPIO8 is the onboard WS2812 on both boards. On the Zero it is *also* on the
back pad row, so an external WS2812 wired there sits in parallel on the same
data line and shows the same colour — an indicator outside a closed case for no
extra pin and no firmware change. See roadmap step 10.

### Motor control (DRV8833)

| IN1 | IN2 | |
|---|---|---|
| 1 | 0 | forward (feed) |
| 0 | 0 | coast |
| 1 | 1 | brake |

Feeding = run forward until N falling edges on the switch, then brake. Stop
**on** the edge, so the hub always parks in the same position. Safety
timeout: no click within the jam budget while running → stop, report `jammed`.
That budget is derived from the unit's detent interval, 4750 ms on the reference
mechanism.

Switch: **GPIO2**, internal pull-up enabled in software
(`InputConfig::default().with_pull(Pull::Up)`), other contact to GND. No
external resistor. Idle reads high, pressed reads low, so a press is a
**falling** edge. Debounce 30 ms in software (8 rpm → one edge every ~1.9 s,
bouncing is trivial to filter).

GPIO2 is on the DEV-KIT's J1 header, which reads `5V · GPIO3 · GPIO2 · GPIO11`,
so moving the bench jumper off the old GPIO11 is a shift of one position.

⚠️ The nearest ground, J1 pin 15, sits **directly beside 5V**, and so does
GPIO3, which the reset button now uses. A ground jumper off by one position
puts 5 V onto a signal pin and destroys it. Either double-check that jumper or
take a ground from the J3 header, which has no 5 V neighbour.

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

**Minimum spacing between clicks lives in `feeder.rs`, not in `switch.rs`, and
is derived per unit** — 760 ms on the reference mechanism. Just after the motor
starts, the hub is sitting right on an edge; a fraction of a turn can bounce the
switch and produce a spurious falling edge at zero rotation. So inside the
counting loop, an edge arriving sooner than that after the previous one, or
after the motor started, is discarded. The threshold is two fifths of a detent,
so it cannot reject a real click at any mechanism speed. It works together with
the 30 ms debounce, not instead of it.

The placement matters. That detent floor only holds **while the motor is
driving**. A bench button pressed twice quickly produces edges far closer
together and every one of them is real. If the rule lived in `switch.rs`, the
stream would silently swallow them and lie about what it observed, and every
bench test would look like a broken debounce.

So: `switch.rs` debounces at 30 ms and reports **every** real edge.
`feeder.rs` applies the spacing rejection, where the motor-driven assumption
actually holds.

The no-edge timeout remains the jam guard, derived the same way at two and a
half detents.

These four cases — *starts pressed*, *starts free*, *bounce at t=0*, *no clicks
at all* — are host tests in `feeder.rs`, along with the one that is easiest to
get wrong: repeated bounce must not postpone jam detection.

Note the threshold is a wide margin, not a check that a full detent happened.
Real contact chatter lasts milliseconds; a real detent takes the interval this
unit was calibrated for. Anything in between cannot occur while the motor
drives, so the threshold sits in the empty middle rather than close to either
edge — and `feeder.rs` has tests asserting it stays there for every interval
from 1 ms to 5 s, rather than only for the mechanism on the bench.

### The feeder task owns the motor

One task, one queue. Producers (`mqtt`, `schedule`) send portion counts and
nothing else; only this task touches the motor and the switch, so there is no
shared mutable state and no mutex.

**The decisions live in a pure state machine, `feeder::Feeder`, not in the
task.** Time arrives as milliseconds in each call, so the machine needs no
clock and no executor and is fully host-tested. The task asks what to do, does
it, and reports back. It decides nothing.

```rust
// producers send *portions*: FEED.try_send(2)
static FEED: Channel<CriticalSectionRawMutex, u8, 8> = Channel::new();

// Both from this unit's record in flash: one binary, three mechanisms.
let mut feeder = Feeder::new(cfg.timings, cfg.portion_scale_pct);
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

  reset button (GPIO3) held → erase the record, reboot    (lands in "missing")
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

GPIO2 is the rotor microswitch, inside the mechanism and unreachable once
assembled. The reset button is a separate part on **GPIO3**, and goes somewhere
you can press it.

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

The reset button is already built — see *The outside button* below. It erases at
power-on rather than at runtime, so by the time setup mode exists the "no valid
record" state is reachable without any further work.

### Credentials: getting them out of the binary

**The tool is built and verified; the deletions still wait on setup mode.** The
goal is one mechanism that serves development and production, with the Wi-Fi
password never compiled into the firmware at all.

The temptation is to keep `seed_config` and formalise it — cfg.toml supplies
defaults, flash is seeded at first boot, the reset button wipes. It works, and
the dev loop is pleasant. But it makes compiled-in credentials permanent, which
is the exact thing this step exists to remove, and it leaves a release binary
carrying a Wi-Fi password for a house it may never be installed in.

**Write the record from the host instead.** `provisioning::Record::encode` is
pure and already host-tested, so the same code the firmware uses can produce the
bytes on a laptop, and `espflash` can put them straight into the `nvs`
partition — the one `espflash` otherwise never touches, which is exactly why
configuration already survives a reflash.

```sh
./dev/provision.sh            # cfg.toml -> record -> flash, once per board
cargo run                     # forever after; credentials are already there
```

Three pieces:

1. **`examples/mkrecord.rs`**, built for the **host**, not the board. An example
   rather than a second `[[bin]]`, because `[[bin]]` is the firmware and is
   board-targeted; examples compile for the host since `provisioning.rs` sits
   above the gate in `lib.rs`. It reads `cfg.toml` with the `toml` crate — a
   dev-dependency, mirroring the existing build-dependency — and writes
   `MAX_RECORD_LEN` bytes, padded with `0xFF` so the image is deterministic and
   matches what erased flash looks like around it.
2. **`dev/provision.sh`** — build the record, erase one sector, then
   `espflash write-bin 0x9000`. `espflash` takes an address only, with no
   `--partition` flag, so 0x9000 is written in the script; it is the default
   table's `nvs` offset, and the firmware prints its own answer at boot
   (`store: nvs at 0x9000, 24576 bytes`) so the two can be checked against each
   other rather than assumed.

   ⚠️ **`write-bin` does not erase, and NOR flash can only clear bits**, so
   writing a record over an existing one ANDs the two together. Found the hard
   way: `FDR2` written over `FDR1` becomes `FDR0`, and the firmware then says
   `store: no record yet` — which looks exactly like the write having silently
   failed rather than like corruption. `espflash erase-region 0x9000 0x1000`
   first is what makes it work, and it is why that step is in the script rather
   than being tidied away as redundant.

   The record holds the Wi-Fi password in the clear, so the script builds it
   into a `mktemp` file and deletes it on the way out rather than leaving it in
   the working tree. `record.bin` is git-ignored as a backstop.
3. **The deletions**, exactly as roadmap step 9 already lists them:
   `seed_config`, `load_config`, `Config::to_record`, `parse_u16`, the key loop
   and CI placeholders in `build.rs`, and the six credential lines in
   `cfg.toml.example`. `cfg.toml` keeps the credentials, but only as input to
   `provision.sh` — they never reach a compiler. `ap_secret` stays build-time,
   because it is a salt rather than a credential and the firmware must derive
   the same AP password the sticker shows.

**Sequencing, and the one thing that blocks.** Pieces 1 and 2 are done and
verified on the dev kit — provisioned, reflashed, and still reading
`store: configured for ...` with no `seeded from cfg.toml` after it. Piece 3
cannot land until setup mode exists, because deleting the fallback leaves an
unprovisioned unit with nowhere to go.

Until then both paths coexist: a provisioned board uses its record, and an
unprovisioned one still falls back to `cfg.toml`. The absent `seeded from
cfg.toml` line is the only thing that distinguishes them, which is why it is
the documented check.

**What this also buys.** The same script provisions the three Zeros without ever
raising an access point or typing on a phone, which makes step 6 a good deal
less tedious, and it is the natural way to re-provision a unit whose Wi-Fi
password changed while it is still on the bench.

**To verify:** provision a board, reflash the application, and look for
`store: configured for ...` with **no** `store: seeded from cfg.toml` line after
it. That single absent line is the whole proof.

### Per-unit mechanical timing

**Built and verified.** Changing a unit's calibration is a `provision.sh` flag,
not a rebuild.

Three units, and one of them a different brand, means the mechanical timings
cannot stay compile-time constants. But they must not go in `cfg.toml` either:
that is build-time, so per-unit values there mean **a different binary per
unit**, and one binary flashing every unit is what makes the MAC-derived device
id worth having.

The record in flash is the right home. It is already per-unit, already written
by `provision.sh`, and already read before anything else at boot.

**Measure one number, derive the rest.** The only thing worth observing on a
bench is the **detent interval** — how long the motor takes to get from one
click to the next. Both other constants follow from it, and today's
hand-picked values are very close to what these ratios produce:

| Constant | Rule | At 1900 ms | The old hand-picked value |
|---|---|---|---|
| minimum click spacing | interval × 0.4 | 760 ms | 800 ms |
| jam timeout | interval × 2.5 | 4750 ms | 5000 ms |

That agreement is the argument for the ratios: they are not invented, they are
what the working mechanism already implies. Deriving also keeps the property
that matters — the spacing threshold has to sit in the empty middle between
contact bounce (milliseconds) and a real detent — automatically, at any speed,
instead of needing to be re-reasoned per unit. `feeder.rs` pins both halves of
that over every interval from 1 ms to 5 s: never within 4× the debounce, and
never so wide that a real detent is rejected.

Both have floors for a hypothetically fast mechanism, expressed against
`DEBOUNCE_MS` rather than picked freely. That constant now lives in `feeder.rs`,
with `switch.rs` deriving its `Duration` from it — the gated module depending on
the pure one, rather than two copies of 30.

**It did not make `feeder.rs` impure.** `Timings` and the scale are parameters
on `Feeder::new`, a change in signature rather than in shape, and the tests got
better for it: they now exercise a fast mechanism and a slow one instead of only
the one on the bench.

The console says what a unit was calibrated for, once at boot, because a feeder
behaving oddly is either mis-measured or mis-provisioned and nothing else tells
them apart:

```
INFO - feeder: clicks >760 ms apart, jam after 4750 ms, portions x100%
```

Verified end to end: `./dev/provision.sh --detent-ms 900 --portion-scale 133`
and the same binary comes back with `clicks >360 ms apart, jam after 2250 ms,
portions x133%`.

```sh
./dev/provision.sh                      # the cfg.toml default
./dev/provision.sh --detent-ms 900      # the odd one out
```

### Per-unit portion size

**Built**, in the same record as the timing above.

`feeder/schedule` is one retained topic shared by all three units, so a slot
saying `portions: 2` reaches every feeder as the same request. The feeders are
not all the same model, and a click on one mechanism need not dispense the same
amount of food as a click on another. Without a per-unit scale one feeder
over- or under-feeds forever, and **nothing in the system can see it** — Home
Assistant sees every request succeed.

So: a `portion_scale_pct` per unit, 100 meaning unchanged. Three portions
becomes four clicks at 133%, or two at 67%.

**Portions are the contract; clicks are the mechanism.** Everything arriving
from outside speaks portions — the Home Assistant button, `feeder/<id>/feed`,
`feeder/all/feed`, every schedule slot — and `clicks_for` is the single place
they become clicks. Everything downstream of it counts clicks, `MAX_CLICKS`
included, which is correct for a cap whose job is protecting the hopper: what
empties a hopper is clicks, not intentions.

`MAX_PORTIONS` is now `MAX_CLICKS`, and **raised from 10 to 16**. Ten was the
old portion cap and the two happened to be the same number; once a scale exists
they are not. A unit at 150% asked for ten portions wants fifteen clicks, and
clamping back to ten would silently under-feed the one unit most likely to need
a scale in the first place.

Two rules worth knowing before reading the code:

- **A request for one or more portions never becomes zero clicks**, at any
  scale. Rounding a meal away is a feeder that silently stops feeding, which is
  the failure this whole project is built to avoid. `feed 0` still means zero,
  because it is a documented no-op rather than a meal.
- **Rounding is to nearest and per request; no remainder carries between
  meals.** A carried remainder would make the same slot give two clicks some
  days and one on others — unreadable on a console, and awkward against the
  never-double-feed guard. The price is that small counts only approximate: at
  133%, a one-portion meal is one click, not 1.33. If that matters for a unit,
  the fix is a schedule with larger counts, not cleverer rounding.

Report `last_fed` and the state payload in **portions as requested**, not in
clicks. Home Assistant asked in portions and should be answered in the same
units, or its history stops matching its own automations.

### The outside button

GPIO3, outside the case, distinct from the hub microswitch on GPIO2 which is
sealed inside the mechanism. Two runtime gestures and one boot gesture, all in
`button.rs` as pure logic.

| Gesture | Effect |
|---|---|
| hold 2 s | **arm**. LED blinks cyan twice a second |
| tap while armed | feed one portion, and refresh the window |
| tap while locked | nothing, but says so on the console |
| nothing for 10 s | locks again |
| **held through power-on, 3 s** | erase the record |

**The adversary is cats, not clumsiness.** A button on the outside of a cat
feeder that dispenses food when pressed is a button cats will learn to press —
food is the strongest reinforcer there is and a cat has all day to experiment.
That is the whole reason for arming, and it is why a tap alone does nothing.
**Recess the button** as well: needing a fingertip defeats a paw outright, and
mechanical protection cannot be got round by a lucky sequence.

**Reset is a boot gesture on purpose.** Sharing one button between feeding and
erasing means separating them by hold duration, and the failure mode writes
itself: hold a beat too long on a working feeder and its credentials are gone,
with three units already screwed into place. Requiring a power cycle means it
cannot happen by accident at all. It costs one GPIO read on an ordinary boot —
only a boot that begins with the button held waits the three seconds.

Arming outranks every fault on the LED. The case that settles it: the broker is
down, which is precisely when manual feeding matters, and being re-told the
network is out is less useful than seeing that the tap will land. Whatever it
hides is still there ten seconds later.

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
| `MAX_CLICKS` (`portions.rs`) | 16 | clicks owed at once, so the most one meal can turn |
| `FEED_DEPTH` (`wiring.rs`) | 8 | unread feed **requests** in the channel |

Two meals a day is the usual case, but three, four or five are ordinary and all
fire. A schedule with more than `MAX_SLOTS` entries is rejected whole rather
than truncated, because a silently shortened one drops meals with nothing to
show for it, and the unit keeps running the schedule it already had.

`MAX_CLICKS` caps a single meal, not the day, because the queue drains between
them. It counts clicks rather than portions: `portions::clicks_for` runs first,
in `Feeder::request`, so a unit with a portion scale is capped on what it
actually dispenses. `FEED_DEPTH` counts
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
motor*. `MAX_CLICKS` is 16, clamped with a warning, so a stuck automation
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

## The RGB LED

The onboard WS2812 on GPIO8 is the unit's second output channel, and the only
one that survives the network being the broken thing. Full reasoning is in
`indicator.rs`'s module docs; the rules that matter from outside:

| State | LED |
|---|---|
| jammed | **solid** red |
| feeding | **solid** white |
| button armed | cyan, one flash every 0.5 s |
| setup mode (step 9) | blue, one flash every 2 s |
| no Wi-Fi | red ×1 every 3 s |
| no broker | red ×2 every 3 s |
| no trusted time | red ×3 every 3 s |
| paused | amber ×1 every 5 s |
| healthy | green ×2, **then dark indefinitely** |

Three things here are decisions rather than taste:

- **Dark is healthy.** If lit were the normal state, lit would carry no
  information and nobody would look at it. The cost — dark no longer separates
  healthy from dead — is mostly paid back by a brown-out reboot replaying the
  green confirmation, so a boot loop reads as a repeating double flash.
- **Faults are counted, not coloured.** One, two and three point at the router,
  the broker address, and Home Assistant's publish automation — three different
  fixes. Counting flashes works across a dark room and for a colour-blind
  reader; distinguishing amber from orange through a diffuser does not.
- **Solid means the mechanism, blinking means the network.** That is what keeps
  a jam unambiguous without a fourth count nobody could count.

Green flashes on *entering* the healthy state, so it also marks a feed
finishing cleanly and a dropped connection coming back.

**The wire order is RGB, not the GRB the WS2812B datasheet specifies.** That is
empirical, from the dev kit: sending GRB inverted the whole palette, so every
red fault code blinked green and the healthy confirmation flashed red — with the
console still cheerfully logging `led: Jammed` next to a green LED. The two
boards are not guaranteed to carry the same part, so the power-on sweep names
each primary as it shows it and settles the question on a Zero in one flash.
`led::wire_word` is the single place to change it.

**Getting it outside the case costs nothing.** On the Zero, GPIO8 is on the
back pad row as well as being the onboard LED's DIN. An external WS2812 wired
there sits in parallel on the same data line — both parts latch the first 24
bits and show the same colour. No second pin, no second RMT channel, no
firmware change, which is why `led.rs` sends 24 bits and not 48.

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
  bin/main.rs     wiring: peripherals, tasks, executor
  board.rs        pin map per board (feature-gated)
  motor.rs        Motor { run_forward(), brake() } over two Output pins + nSLEEP
  switch.rs       debounced click stream (async), 30 ms; reports every edge
  feeder.rs       owns motor + switch; FEED queue, align, per-unit spacing,
                  count, brake, jam timeout, portions -> clicks
  schedule.rs     pure logic: Schedule, LocalClock, next_due(), double-feed guard
  portions.rs     pure logic: the pending-click counter, its cap, and the
                  per-unit portions -> clicks conversion
  button.rs       pure logic: what a press of the outside button means
  indicator.rs    pure logic: what the LED shows, the priority ladder, the
                  blink timing
  led.rs          the WS2812 itself, over RMT. Colours in, bits out
  mqtt.rs         connection, LWT, discovery, subscriptions, state publishing
  wiring.rs       the Bus static's types: FeedChannel, FeederStatus, LastFed,
                  Connectivity
  provisioning.rs pure logic: the flash record, setup-network credentials,
                  the setup form and just enough HTTP
  sha256.rs       pure logic: SHA-256, shared with dev/ap-password.sh
  store.rs        reads and writes the record in the nvs partition
  config.rs       Config, from a flash record or the build-time fallback
build.rs          injects cfg.toml values as env vars (the fallback; on its way out)
examples/mkrecord.rs
                  host-only: builds a provisioning record for dev/provision.sh

homeassistant/packages/cat_feeder.yaml
                  the other half of the system: publishes time and schedule,
                  the pause helper, the feed-all script. Tracked here and used
                  unchanged on the Pi; install per dev/README.md
```

Embassy tasks: `net` (Wi-Fi + stack), `mqtt`, `switch` (owns the GPIO),
`feeder` (owns the motor), `schedule` (owns the clock, ticks once a second and
re-aligns), `indicator` (owns the LED). They communicate through the one
`wiring::Bus` static, which names every shared handle and documents who writes
each one.

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
   logging fake motor (align, spacing rejection, counting, jam, accumulation).
   Still to do: the DRV8833, and with it the one measurement that matters —
   the **detent interval**, the time from one click to the next under power.
   That belongs here rather than in step 2: the hub can be back-driven by hand,
   but the gear reduction makes turning it steadily impossible, so a hand-turned
   interval is meaningless. The motor gives it at the speed the mechanism
   actually runs at.
   Clicks per revolution is no longer part of this. Nothing counts revolutions;
   it was only a way to infer the interval from rpm, and the interval is
   measured directly. A full turn giving a *stable* count is still worth
   checking once, as a way to catch missed or doubled clicks
4. ✅ Wi-Fi + MQTT: connect, LWT, availability, discovery (button + switch +
   binary_sensor), subscriptions, manual and broadcast `feed`, `paused`, and a
   state payload carrying the feeder's real flags
5. ✅ `schedule` + `time` handling, local clock, double-feed guard. Pure logic
   in `schedule.rs` with 32 host tests, and every rule verified on hardware by
   driving `feeder/time` from the broker
6. ✅ Board feature: `board-devkit` (default) / `board-zero`, selecting the pin
   map, the board name and `esp-println`'s interface (`uart` vs `jtag-serial`).
   Both variants build and lint; the dev kit path is verified on hardware.
   The Zero's pad map has now been checked, and it cost the two pins the design
   had picked: **GPIO10 and GPIO11 are not brought out on that board**, so the
   switch moved to GPIO2 and the reset button to GPIO3, on both boards rather
   than diverging. Still to do: flash the three production units
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
never guess*. Home Assistant cannot be told — it is the thing that is down — so
this used to be visible only on a serial console. It is now **three red flashes
on the LED**, which is the whole reason step 10 exists.

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
   - ✅ the reset button on GPIO3, as a **boot** gesture rather than a runtime
     one — see *The outside button*. Erases today; becomes a true reset when the
     fallback below goes, since `seed_config` currently writes it straight back
   - ✅ `examples/mkrecord.rs` + `dev/provision.sh`, writing the record from the
     host. Verified on the dev kit: provisioned, reflashed, still configured
     from flash. See *Credentials: getting them out of the binary*
   - ✅ `FDR2`: the record now also carries the two per-unit mechanical figures,
     so one binary can drive three different mechanisms. Stored and round-tripped;
     `feeder.rs` does not consume them yet, which needs the bench measurements
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

10. Status LED. Independent of every other step. See *The RGB LED* above.
    - ✅ the pure layer: priority ladder, patterns and blink timing, 19 host
      tests in `indicator.rs`, including counting the flashes back out of a
      rendered pattern so the LED cannot claim a code it does not show
    - ✅ the WS2812 over RMT (`led.rs`), hand-written rather than pulling in
      `esp-hal-smartled`, which pins to HAL versions the way `esp-storage` does
    - ✅ the three facts the LED needed that no other task could see —
      association, the broker connection and clock trust — lifted onto
      `wiring::Connectivity`
    - ✅ verified on the dev kit, by eye: the power-on sweep, red ×1/×2/×3,
      amber for paused, and solid red for a jam. Each `led:` line lands 6–20 ms
      after the event that caused it, and the state topic confirms the feed and
      jam transitions independently of the console.
      Solid white and the green ×2 confirmation were not separately eyeballed
      and do not need to be: white is `(16,16,16)`, so no channel order can
      change it, and that green is the same one the sweep shows correctly
    - ✅ a red/green/blue sweep at power-on (`led_selftest` in `main.rs`). Kept,
      not a leftover: with dark as the healthy state, a dead LED otherwise looks
      exactly like a unit with nothing to report, and this is the only moment
      that distinction is made
    - ⬜ tune the palette once a unit is in a kitchen. The constants in
      `indicator.rs` are dim on purpose but were picked by eye, and green reads
      much brighter than blue at the same number
    - ⬜ an external WS2812 on the Zero's GPIO8 pad, in parallel with the
      onboard one. Needs no firmware change — see *The RGB LED*
    - ⬜ wire `Health::setup` when step 9's access point lands. It is the one
      field `Bus::health()` hardcodes to `false`, and it is hardcoded rather
      than kept as an always-false atomic so it cannot read as live wiring

Steps 3, 6 and 8 wait on hardware rather than on code:

| Blocked step | Waiting for |
|---|---|
| 3, the DRV8833 and the clicks-per-revolution contract | the part |
| 6, flashing the three Zeros | the boards |
| 8, retiring the PCBs | 3 and 6 |

Later (not now): a short press on the GPIO3 button feeding one portion, so a
manual feed works with the broker down; battery backup.

**A display, if a part can be found that fits.** The original LCD window is
40 × 18 mm, which points at a 0.91" 128×32 I²C OLED — roughly a 38 × 12 mm
module, two pins, a 512-byte framebuffer, and two lines of about 21 characters.
The common 0.96" 128×64 is the wrong shape: its module is near enough square at
27 mm tall and will not go in.

What sells it is setup mode. A unit currently cannot tell you the password of
the network it just raised, which is the whole reason for the salted derivation,
`dev/ap-password.sh` and printing stickers before first power-on. A screen says
it directly:

```text
cat-feeder-db0260        no broker           waiting for time
DAKS-2W9X-NVQG           192.168.68.108      HA not publishing
```

The salt is still needed — it is what stops a stranger deriving the password
from the MAC in the beacon — but the sticker drops from required to backup.

It does not make the LED redundant: at 0.91" you read a screen standing at the
feeder, while the LED answers *is anything wrong* from the doorway. Three things
to settle first: OLED burn-in over years of showing `next 08:00` (blank it, and
wake on the GPIO3 button — which then collides with short-press-to-feed above,
so those need splitting), that the same blanking handles night glare, and that
the split stays the same as everywhere else in this codebase — a pure layer
deciding *what to show*, host-tested, and a gated task that pushes pixels.

**Dropped, after investigation: sound.** The feeder's `cicalino` turned out to
be a *loudspeaker*, not a buzzer — mylar cone, `SPK+`/`SPK−` on the original
board, and a `Play\REC` button on the front for recording a voice clip. So it
cannot be driven from a GPIO at all (8 Ω would ask for ~400 mA) and would want
a transistor at minimum or an I²S Class-D amp for anything better. The argument
that the cats are already conditioned to it dies with that discovery: a recorded
clip is not cheaply reproducible, the conditioning breaks either way, and cats
relearn a food cue in days. Not worth the parts.

Also later: a configured feeder timezone (`Europe/Rome`) so the unit can apply
the offset itself and work out DST, instead of assuming it shares a timezone
with the broker. Worth doing only if the broker ever publishes UTC, or moves to
a different zone from the feeders. It means carrying timezone rules on the
device, which is precisely the weight the current design avoids, so it is a
deliberate trade rather than an obvious improvement. The offset is already
parsed and kept in `Wall::offset_minutes`, so the input is there when needed.
