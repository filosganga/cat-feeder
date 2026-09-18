# cat-feeder — ESP32-C6 firmware (Rust, no_std, Embassy)

Replacement electronics for three commercial automatic cat feeders. The
original PCB (LCD + RTC + buttons) is removed; the mechanics (5 V DC geared
motor + microswitch on the output hub) are kept. Three units must feed at the
same instant, coordinated by Home Assistant over MQTT.

## Hardware (per unit)

| Part | Notes |
|---|---|
| Waveshare ESP32-C6-DEV-KIT-N8-M | **dev board only** (breadboard, pin headers). WROOM-1 module, 8 MB flash |
| Waveshare ESP32-C6-Zero ×3 | **production boards**, one per feeder. Bare C6, **8 MB flash — measured, not read off the schematic**, which shows an `ESP32-C6FH4` and would have you believe 4 MB. `espflash board-info` on unit `99177c` reports 8 MB, and the chip is the authority |
| DRV8833 breakout (black 10-pin) | H-bridge. `nSLEEP`/`ULT` **must be driven high** or the motor won't run |
| Motor DRF-W500CA, 5 V, 8 rpm | geared reducer → stops dead on brake, no coasting past a detent; ~1.9 s between detents. Back-drivable by hand, but stiff enough that turning the hub is a poor way to test anything |
| Microswitch on output hub | **1 click = 1 portion.** That is the entire contract |
| 220 µF 16 V electrolytic | across 5 V/GND next to the DRV8833 (brown-out on motor start) |
| 5 V from the feeder's original USB port | ≥1 A adapter. **No batteries in v1** |

### Two supplies, and one of them is a laptop

A feeder on the bench can have its own 5 V adapter *and* a USB cable to a
laptop, and the two meet at the Zero's `5V` pad. **That is safe, and the board
is why.** The ESP32-C6-Zero carries a **B5819WS Schottky, `D1`**, between the
USB connector's `VBUS` and `VCC_5V` — and `VCC_5V` is both the regulator's input
and `P8` pin 1, the `5V` pad. It conducts `VBUS → VCC_5V` only, so feeding the
pad cannot push current back into the laptop. **Do not fit an external diode;
there is already one.** (Waveshare's published schematic, sheet 1, the `USB`
block. The orientation follows from function rather than from pin numbering: the
board runs from USB, so `D1` must conduct that way and must block the other.)

**~4.8 V on the `5V` pad is that diode, not a sagging supply.** A B5819WS drops
about 0.2 V at the current an idle C6 draws, so a port at ~5.05 V reads ~4.83 V
at the pad — measured. It looks like a fault and is not one; the ME6217C33M5G
behind it needs far less headroom than that. Do not try to infer the diode's
presence *from* that number in the other direction, either: a direct connection
through a lossy cable lands in the same place, which is why the schematic is
what settles it.

With the adapter on, the rail sits above `VBUS − Vf`, `D1` stops conducting, and
the adapter supplies everything while the laptop supplies nothing. During the
motor's inrush the rail dips, and if it falls far enough the laptop briefly
helps through `D1` — milliseconds, well inside the part's surge rating.

**Ground is shared, and must be.** The DRV8833's `AIN1`, `AIN2` and `nSLEEP` are
referenced to the Zero's ground, so the adapter's ground, the driver's and the
board's are one node. Sharing ground while the 5 V rails are separately sourced
reads as contradictory and is not.

The rail itself is one node with three taps — adapter, `VM`, and the Zero's
pad — so motor current flows adapter → rail → `VM` and never crosses the pad.
The pad is a load on that node, not a link in the path.

⚠️ **The one combination to avoid is provoking a jam on USB alone.** With no
adapter, the motor's current *is* drawn through `D1` and `P8` pin 1. That is
fine for a running motor at a couple of hundred milliamps — it is how every
bench test so far has run — but a stall held for the whole jam budget is more
than a 1 A Schottky should carry. Plug the adapter in before testing a jam.

Both supplies live is also exactly what the **detent interval** measurement
needs: the motor turning a real mechanism on its real adapter, with the console
on USB.

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

### The electronics live in their own case

**Decided, not built.** The original plan was to reuse each feeder's own LCD
window and button hole. That is what *A display* below is still written against,
and it is why three lines of 21 characters appears there as a hard constraint.

It does not survive the third feeder being a different brand. Two units share a
window and a button position; the third does not, so reusing them means **two
mounting designs for three units** — and the one unit whose interior nobody has
seen yet is the one that would set the second design.

So: a **separate 3D-printed enclosure** holding the Zero, the display, the
driver and whatever front panel the unit ends up with. Each feeder gets one
hole in its **bottom shell** for the motor and switch cables, routed out
through the cavity the original USB lead already uses. One printed design fits
all three, because it is not fitted to any of them.

What this changes, and each is worth chasing down where it is written:

- **The 40 × 18 mm window stops being a constraint.** Nothing has to fit it,
  so the 1.3" 128×64 already on the bench can be the production part and the
  0.91" parts become the fallback. `oled.rs` is already parameterised — the
  `panel-128x64` feature swaps `DisplaySize128x32` for `DisplaySize128x64` and
  nothing else moves.

  ⚠️ **That buys rows, not columns.** Both panels are 128 pixels wide, so
  `FONT_6X10` gives **twenty-one characters on either**, and `display::COLS`
  stays 21 — the feature does not touch it. What changes is `ROWS`: 64/10 is
  six lines instead of three. So three lines becomes a floor and **21 columns
  remains a hard ceiling**, which is the half that bites: `Line` is
  `String<COLS>` and `push` truncates in silence, with no log line and no
  failing test. A menu laid out against "the budget is gone" loses the tail of
  every long line on the glass.
- **A hole in a printed part costs nothing.** That is what makes the knob a
  v1.5 question rather than a step-8 deadline: a case can be reprinted, a
  commercial shell drilled wrong cannot be undrilled. See *Version 1.5: the
  knob* below.
- **The front panel need not be within a cat's reach.** A separate box can sit
  higher, or behind the feeder, which is a placement freedom a panel bolted
  into the original window never had.
- **Step 8 changes shape**, from transplanting electronics into three different
  interiors to drilling one hole and routing a cable in each. The mechanical
  figures — 1 click = 1 portion, and the detent interval — are unaffected:
  they are about the hub, not about where the board lives.

What it does **not** change: the third feeder still has to be opened, still has
to have its detent interval measured, and still has to be confirmed to have a
microswitch rather than an optical sensor. Those are in the mechanism.

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
the back pads — **thirteen usable, seven of them spent today**, so the display
fits with room left. **`board.rs` carries the assignment table**, which is the
thing to solder against; it is not repeated here.

Seven and not eight: this design wires eight pins, but GPIO8 is not one of the
thirteen — it is struck out twice in the table above, as strapping and as the
onboard WS2812, so it was never available to spend. Count the free pins against
seven or the arithmetic comes out one short.

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
is derived per unit** — 760 ms on the reference mechanism. Braking parks the hub
*on* an edge, so a run that starts with the switch already closed can chatter
out a spurious falling edge at zero rotation. So inside the counting loop, an
edge arriving sooner than that after the previous one, or after the motor
started, is discarded. The threshold is two fifths of a detent, so it cannot
reject a real click at any mechanism speed. It works together with the 30 ms
debounce, not instead of it.

**The align phase is exempt, and that is not a detail.** The rule above needs
the hub to be resting on a detent, which is exactly what a run starting with the
switch *open* tells you it is not: the rotor is somewhere unknown between
detents and may be a hair short of the next one, so its first genuine edge can
arrive at any time. Rejecting an early one throws away the real alignment click
and spends another whole detent finding the next — **and that quarter turn
dispenses food that nothing counts**, which is an over-feed on the one path the
never-double-feed guard does not cover.

So while aligning, any debounced edge is accepted and the jam timeout is the
only bound. It is also the only bound that can be justified, because nothing
about the rotor's position is known. Contact chatter is already handled a layer
down by the 30 ms debounce; the 760 ms figure was only ever about the
start-on-a-detent case.

This was found on a bench, by hand, and the console named it: repeated
`feed: edge ignored, below 760ms minimum spacing` while alignment never
completed.

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
- **Wi-Fi + MQTT credentials come from flash and nowhere else.** They are not
  compiled into the binary: `dev/provision.sh` writes a record over USB, or the
  setup form writes one over the unit's own access point. `Config` is what the
  firmware consumes either way. The one build-time value left is `ap_secret`,
  which salts the setup password and is not a credential for any network.
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
and serves a form. **Built**, and driven end to end from a phone: typed in,
saved, rebooted, joined the house network and reached the broker.

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
so stickers can be made before a unit is first powered on; in setup mode the
firmware prints it on the console and shows it on the panel as well. **All of
them must agree byte-for-byte**,
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

### How it was built

All four steps are done and driven end to end from a phone. Kept rather than
deleted because the API facts cost an hour to establish and are not obvious
from the code that resulted; the notes under each step are what would otherwise
have to be rediscovered.

**A gated module, `setup.rs`, entered from the boot path when there is no
usable record.** It never returns — it reboots once a record is saved, so the
normal path always starts from a clean boot. Its module doc counts the *network*
in three slices rather than four, because raising the stack and serving DHCP are
one thing to verify: a phone either gets an address or it does not. A fourth
slice was added later for the panel, which is not a network slice at all and is
owned by `main.rs` — see *A display* below.

1. ✅ **Raise the access point.** Build `AccessPointConfig` with `Wpa2Personal`
   and the SSID and password `main.rs` derived from `ap_ssid(id)` and
   `ap_password(AP_SECRET, id)` — they arrive as two `&str` rather than being
   worked out here, because the screen has to show the same two strings. Then
   `esp_radio::wifi::new(wifi, ControllerConfig::default()
   .with_initial_config(WifiConfig::AccessPoint(..)))`. There is **no separate
   start call**: `set_config` calls `esp_wifi_start()` whenever the mode
   changes, so applying the initial config brings the network up. Keep the
   controller alive for as long as setup mode runs.
2. ✅ **Bring up a second stack** on `interfaces.access_point`, which is an
   ordinary embassy-net `Interface`. `Config::ipv4_static(StaticConfigV4 {
   address: 192.168.4.1/24, gateway: None, dns_servers: empty })` and its own
   `StackResources`.

   **Not** the existing `net_task`, as this plan first said: that one is
   defined in the binary crate and `setup.rs` is in the library, so a library
   module cannot spawn a task it cannot name. `setup.rs` has its own, two lines
   long.
3. ✅ **Serve DHCP**, or a phone joins and gets nothing. `edge-dhcp` is a codec,
   not a server: `Server::handle_request` takes a parsed `Packet` and returns
   one to send, and the packets are moved by an embassy-net `UdpSocket` bound
   to port 67. The pool is 192.168.4.2–192.168.4.9.

   Two things the plan did not say, both decided at the bench:

   - **`Server::new` defaults its pool to `.50`–`.200`**, which is not this
     one. `range_start` and `range_end` are public fields and are set after
     construction.
   - **The `gateway: None` above is the *unit's* routing table, not what
     clients are told.** The DHCP server advertises the unit as the client's
     gateway even though it forwards nothing, which is what every ESP-IDF
     softAP does: a phone handed no router at all can decide the network is
     broken and drop it, whereas one that routes at us simply finds its packets
     go nowhere — which is true, and the point. No DNS server is advertised,
     because there is not one and hijacking lookups is the captive portal this
     design has already declined.

   **Association is logged separately from DHCP**, and that is not decoration.
   A capture with nothing in it cannot otherwise distinguish *the phone never
   joined* from *the phone joined and DHCP is broken*, and those have nothing
   in common to debug. This cost one wasted capture to learn. `wifi::new`
   already enables the access-point station events, so it is a subscription and
   no configuration.
4. ✅ **Serve the form** on TCP 80. `provisioning::parse_head` reads the request
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

The temptation was to keep `seed_config` and formalise it — cfg.toml supplies
defaults, flash is seeded at first boot, the reset button wipes. It worked, and
the dev loop was pleasant. But it makes compiled-in credentials permanent, which
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

**All three pieces are done.** The deletions were planned to wait until setup
mode worked, on the reasoning that removing the fallback would strand an
unprovisioned unit. That ordering predates `provision.sh`: with a USB route to
write a record, nothing can strand itself, and the deletions had to come *first*
because `seed_config` refilled flash on every empty boot and made setup mode
unreachable.

**What this also buys.** The same script provisions the three Zeros without ever
raising an access point or typing on a phone, which makes step 6 a good deal
less tedious, and it is the natural way to re-provision a unit whose Wi-Fi
password changed while it is still on the bench.

**To verify:** provision a board, reflash the application, and look for
`store: configured for ...`. An unprovisioned board says
`store: no record yet, going to setup` instead and raises its own network —
there is no third outcome, because there is no fallback left.

That the Wi-Fi password is genuinely absent from the binary is checkable
directly rather than by reading code:

```sh
strings target/riscv32imac-unknown-none-elf/debug/cat-feeder | grep -c "<your wifi password>"
```

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
./dev/provision.sh --host 192.168.68.126  # ...and pointed at the Pi
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
sealed inside the mechanism. Four runtime gestures and one boot gesture, all in
`button.rs` as pure logic.

| Gesture | Effect |
|---|---|
| hold 2 s while locked | **arm**. LED blinks cyan twice a second |
| tap while armed | feed one portion, and refresh the window |
| hold 2 s while armed | **lock** again, without waiting out the window |
| tap while locked | wake the screen, and step through its pages |
| nothing for 10 s | locks again |
| **held through power-on, 3 s** | erase the record |

**Hold toggles the mode; a tap does whatever the mode means.** That is the whole
vocabulary, and it is worth the symmetry: the previous version had no way out of
armed but waiting, and wasted the locked tap on a log line. Three rules keep it
honest:

- **A lock has to come from a different press than the arm.** Arming fires
  *while* the button is still held, so without this a four-second hold would arm
  at two seconds and lock at four, and read as a button that does nothing.
  `Button::armed_this_press` already exists for the neighbouring reason — one
  hold must not arm twice, and its release must not count as a tap — and this is
  the same flag.
- **Waking always shows the first page.** A tap on a sleeping panel lights it
  and shows page one; taps after that advance. Otherwise the first press shows
  whatever page you left it on days ago, which reads as a screen stuck on the
  wrong thing.
- **Cats are unaffected.** Every locked tap is still foodless, which is the
  property the whole design rests on — a cat that learns to press gets a lit
  screen and nothing else, and the screen sleeps by itself.

Locking needs no separate confirmation: a hold is a press, any press wakes the
panel, and the banner stops saying `TAP TO FEED`.

⬜ **Two rows of that table are not built yet** — the lock hold, and a locked tap
stepping the screen. Today a locked tap only logs. Arming, feeding, the ten
second window and the boot gesture all work and are verified on a Zero. See
*The screen's pages*.

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
feeder/time/request        <id>                    cmd to HA, NOT retained
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
(`clock: live time 2026-09-18T00:07:18+02:00, schedule armed`) rather than
dropped: it turns a silent hour-long error into the first line on the console.

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

**Home Assistant finds the units rather than being told them.** Pausing is the
only command with no broadcast topic, so it is one publish per unit and
therefore the one place that needs to know which units exist. It derives them
from the device registry — discovery gives every feeder a device whose `model`
is this firmware's and whose `identifiers` are `feeder_<id>` — so no device id
is written down in `cat_feeder.yaml` and a new unit joins by itself. `model` is
consequently a contract between `mqtt.rs` and the package: change it in one
place and pause silently stops matching anything.

**It publishes to the topic rather than calling `switch.turn_on` on the
discovered switch**, and that is not a stylistic choice. Home Assistant drops
unavailable entities from an entity service call, and every feeder's switch
carries an `availability_topic` — so pausing while a unit is unplugged would do
nothing at all, in the one direction where the failure is cats not being fed.
Publishing always lands, and the broker holds it retained for a unit that is not
listening yet, which is the whole point of the topic being retained.

A feeder left paused is the one failure mode where cats do not eat and nothing
alarms. Keep `paused` visible in the state payload and as a switch in HA. There
is deliberately no automation warning about it: these feeders are paused
precisely when somebody is home to feed by hand, so the notification would fire
on the normal case and be trained away. `dev/README.md` says what to add for a
feeder that normally runs unattended.

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
each primary as it shows it.

**Both boards are RGB — settled by eye on the first Zero**, which showed red,
then green, then blue in the order the console announced them. So `wire_word`
stays one function rather than becoming board-dependent, which was the fallback
if they had disagreed. It remains the single place to change if a later part
differs.

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
- `./dev/flash.sh [--seconds n] [--filter re] [--board devkit|zero] [--port p]`
  = build + flash + bounded capture, with each
  line annotated by the gap since the previous one. `./dev/capture.sh` does the
  same without reflashing. `./dev/soak.sh [--hours n]` captures overnight and
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
Home Assistant container, the Mac's LAN address from the ESP32 — which is why
`cfg.toml` says `mqtt_host = "auto"` and `dev/provision.sh` resolves it when it
builds the record. That address is a DHCP lease and moves; a unit provisioned
before a move sits flashing red twice, which is correct for "no broker" and
looks exactly like a broker that is down. `down -v` is the only way to test a cold boot, since every piece of
persistent state in this design lives in the broker's retained messages.

## Code organisation

```
src/
  bin/main.rs     wiring: peripherals, tasks, executor
  board.rs        pin map per board (feature-gated)
  motor.rs        the MotorDriver trait, Drv8833 over IN1/IN2/nSLEEP, and a
                  logging stand-in for running the feeder with no driver wired
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
  display.rs      pure logic: the three lines the screen shows, and when the
                  panel is lit
  oled.rs         the SSD1306 itself, over async I2C. Text in, pixels out
  mqtt.rs         connection, LWT, discovery, subscriptions, state publishing
  wiring.rs       the Bus static's types: FeedChannel, FeederStatus, LastFed,
                  Connectivity
  provisioning.rs pure logic: the flash record, the setup network's identity —
                  SSID, password and the address both setup.rs and display.rs
                  are built from — the setup form, the page it renders as well
                  as the body it parses back, and just enough HTTP
  sha256.rs       pure logic: SHA-256, shared with dev/ap-password.sh
  store.rs        reads and writes the record in the nvs partition
  dhcp.rs         pure logic: where a DHCP reply goes, and a MAC's spelling
  setup.rs        setup mode: the access point, its own stack, DHCP, and the
                  sockets the form is served over
  config.rs       Config, from a flash record
build.rs          injects ap_secret from cfg.toml, and nothing else
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
- **Every dev script setting has a flag, and the flag wins over the matching
  environment variable.** `--board`, `--port`, `--host`, `--user`,
  `--password`, `--nvs-offset`, plus the per-run `--seconds`, `--filter` and
  `--hours`. Prefer them: an `ENV=value ./dev/x.sh` prefix changes the start of
  the command line, which is what a permission rule in `.claude/settings.json`
  matches on, so an allow-rule for the script stops covering the call. The
  variables still work, for a port or a broker exported once for a session.
  `dev/_common.sh` holds the shared parsing and says why; the `dev-script`
  skill has the conventions for writing a new one.
- **When a constant becomes configurable, grep the whole repo for its old
  value.** Copies survive in log strings, doc comments, `README.md` and the
  transcripts under `.claude/skills/flash-and-verify/`, and no test can catch
  them because tests do not read log text. This has bitten three times: the
  800 ms spacing, the 5 s jam timeout, and `MAX_CLICKS` still being described in
  portions. The `drift-check` agent exists for exactly this.
- Don't add features the plan doesn't call for (buzzer, display, battery,
  captive portal) without asking.

## Roadmap

1. ✅ Toolchain + blinky on the DEV-KIT
2. ✅ Switch task: debounced clicks on the console, on a bench button
3. `feed(n)`: ✅ state machine host-tested (align, spacing rejection, counting,
   jam, accumulation), and ✅ driving the real DRV8833 — verified on the first
   Zero with the yellow bench button standing in for the hub switch: a feed
   request ran the bridge, the first click aligned, the second counted, and the
   motor braked.
   Still to do: the one measurement that matters — the **detent interval**, the
   time from one click to the next under power, which needs the motor turning
   the actual mechanism rather than a bare shaft on the bench.
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
   than diverging.

   **The first Zero is now flashed and running**, id `99177c`: the console comes
   up over the chip's own USB, the power-on sweep showed red/green/blue in the
   right order, both GPIO2 and GPIO3 read correctly, and the outside button's
   whole gesture chain works — hold to arm, LED cyan, tap to feed. Two units
   still to build.
7. ✅ Home Assistant: automations publishing time (every minute) + schedule,
   the pause helper and a feed-all script, in
   `homeassistant/packages/cat_feeder.yaml`, verified driving a real scheduled
   feed end to end
8. Retire the old PCBs. Per feeder: remove the original LCD/RTC/button board,
   drill one hole in the bottom shell, and route the motor and microswitch
   cables out through the cavity the original USB lead already uses. The
   electronics live in their own printed case — see *The electronics live in
   their own case* — so nothing is fitted to the feeder's interior and the
   same design serves all three, including the odd one out. Take 5 V from the
   feeder's original USB port. The last step in the project and the only one
   with no software in it.

   **The panel and the knob are not decided here**, which they would have been
   under the old plan of reusing each shell's own window. They live in a
   printed part now, so a hole put in the wrong place is a reprint rather than
   a ruined case, and the interface can keep moving after these three holes are
   drilled. `board.rs` reserves GP20/GP21 for an encoder and half-reserves GP22
   for its switch so the pins stay there either way

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

**Since `feeder/time/request`, a healthy connect usually prints only the second
line**, and that is not a regression. The two messages now arrive within a
second of each other, and `Bus::time` is a `Signal` holding one value between
the schedule task's one-second ticks, so the live answer overtakes the retained
replay and the first line never happens. It comes back exactly when it is worth
reading: when nobody answers the request, which is the case this whole
distinction exists for.

The consequence to know about: a unit that reboots while Home Assistant is down
but the broker is up will **not feed at all** until Home Assistant returns.
That is deliberate, and the same rule as *power-cycled and no broker → wait,
never guess*. Home Assistant cannot be told — it is the thing that is down — so
this used to be visible only on a serial console. It is now **three red flashes
on the LED**, which is the whole reason step 10 exists.

### Asking for the time instead of waiting for it

✅ **Built, both halves**, and it did what it was designed to do: on the same
Zero, the schedule now arms **626 ms after the request goes out** instead of
forty-nine seconds later.

```
INFO (12088) - mqtt: subscribed
INFO (12110) - mqtt: asked for the time
INFO (12736) - clock: live time 2026-09-18T00:07:18+02:00, schedule armed
```

The `:18` is the proof it was an answer rather than a coincidence — the
periodic publishes land on the minute boundary, at `:00`.

**The problem it removed.** Home Assistant publishes on `minutes: "/1"`, so a
unit that connects at 23:46:02 waits until 23:47:00 before anything counts as
live. Observed on a Zero: MQTT connected at 12.7 s, schedule armed at 61.7 s.
**Forty-nine seconds of a ninety-second boot spent waiting for a clock tick**,
and the worst case is a full minute.

It is not the broker being slow. The *retained* time arrives in under a second;
it simply does not count, because a retained message proves only that Home
Assistant published at some point, possibly hours ago. That rule is not
negotiable — it is what stops a unit working through a whole day's slots at the
wrong times — so the fix is to make a live one arrive sooner.

**The design.** A new topic, `feeder/time/request`, published by a unit once per
MQTT connection, carrying its device id. Home Assistant answers by publishing
`feeder/time` immediately.

- **On the firmware side**, `mqtt.rs` publishes it as the last step of the
  connection sequence, *after* subscribing — a reply that arrives before the
  subscription is a reply that is missed. Once per connection, never on a
  timer: the point is to collapse the initial wait, and a unit that keeps
  asking is a unit in a reconnect loop making it worse.
- **On the Home Assistant side**, an `mqtt` trigger on that topic was added to
  the *existing* publish-the-time automation rather than a second one being
  written. Two automations publishing the same topic is how they drift.
- **Never retained.** A retained request would be replayed to Home Assistant on
  every one of its own restarts. It is a command, and the same rule as the two
  `feed` topics applies.

**Why this and not simply publishing more often.** `/10` seconds would be a
one-character change, but it is six times the traffic on a retained topic
forever, it still leaves up to ten seconds of waiting, and it does nothing for
the case that actually recurs — a unit reconnecting after the Wi-Fi drops,
which happens far more often than a reboot.

**The safety property survives.** A time published in answer to a request is
still a live publish, and still proves Home Assistant is running *now*, which
is the whole content of the live/retained distinction. It arrives with the
retain flag cleared, exactly like the periodic one, because the subscription
leaves `retain_as_published` off.

**It degrades correctly**, which still matters because the Pi's Home Assistant
does not have the package installed at all yet: a unit whose request nobody
answers simply waits for the next `/1` publish, which is the old behaviour. The
two halves were therefore independent, and a feeder repointed at the Pi before
step 11 installs the package loses the speed-up and nothing else.

**`mode: single` on that automation is fine.** Three feeders rebooting together
send three requests within milliseconds and Home Assistant will drop two of
them — but the one publish that does happen is forwarded live to all three
subscribers, so every unit is served.

### The other ten seconds: a lost DHCP DISCOVER

⬜ **Not built**, and worth more than it sounds, because nothing is waiting on
anything real for the whole of it.

From `wifi: associated` to `wifi: connected`, four captures in a row measured
10015, 10031, 10033 and 10059 ms. Real DHCP latency is milliseconds and varies;
a constant within 60 ms of ten seconds, four times running, is a **timer**. It
is `discover_timeout: Duration::from_secs(10)` in
`smoltcp-0.13.1/src/socket/dhcpv4.rs:134`.

So the first DISCOVER is being sent and lost, and nothing retries until that
expires. The likeliest cause is ordering: `link_up` is set when the interface
reports association — in the same capture, `link_up = true` at 1752 ms and
`wifi: associated` at 1754 — which is *before* the access point will forward
traffic on our behalf. The DISCOVER goes into the void, and the second one, ten
seconds later, always works.

Worth confirming before fixing, because the fix depends on the cause: if it is
ordering, the stack should not start DHCP until the association is genuinely
complete, or should reset the DHCP socket when it is.

9. Provisioning: credentials from flash, setup over the unit's own access
   point. Independent of steps 3, 6 and 8 — see *Provisioning* above.
   - ✅ the flash record: format, CRC, and every single-bit flip and
     interrupted write rejected (`provisioning.rs`, host-tested)
   - ✅ *parsing* a submitted form: `x-www-form-urlencoded` into a record, and
     enough HTTP to read a request line and its `Content-Length`. Nothing
     serves it yet — see the form item below
   - ✅ setup network credentials, and `dev/ap-password.sh` to match
   - ✅ SHA-256 (`sha256.rs`), pinned to NIST vectors and padding boundaries
   - ✅ reading and writing the `nvs` partition (`store.rs`, `esp-storage`
     **0.9** not 0.10 — 0.10 requires an esp-hal 1.2 release candidate).
     Verified: found at 0x9000, seeded, and read back across a full reflash
   - ✅ the boot decision, and `Config` borrowing a record instead of `env!()`
   - ✅ **build-time credentials retired.** `build.rs` now injects one value,
     `ap_secret`, and nothing else; `seed_config`, `load_config`,
     `Config::to_record` and `parse_u16` are gone. Verified directly: `strings`
     on the ELF finds no occurrence of the Wi-Fi password. Deleting the fallback
     was safe only because `dev/provision.sh` exists — an unconfigured board
     always has a route back over USB — and doing it now is what makes setup
     mode reachable at all, since `seed_config` used to refill flash on every
     empty boot
   - ✅ setup mode entered from the boot path (`setup.rs`), the access point
     raised, and the LED showing it. **`AccessPointConfig::default()` is an
     *open* network**, so `Wpa2Personal` is set explicitly — without it the
     salted password protects nothing and the setup session, the one where the
     home Wi-Fi password is typed, is readable by anyone in range
   - ✅ the setup stack and the DHCP server (`edge-dhcp` 0.8, codec only —
     `default-features = false`, so no `edge-nal`). A second embassy-net stack
     on `interfaces.access_point` at 192.168.4.1/24, and a `UdpSocket` on port
     67 moving packets in and out of `Server::handle_request`. Pool
     192.168.4.2–.9.
     **Verified with a phone**, which is the only way it can be: it associated,
     and 811 ms later took 192.168.4.2 over Discover/Offer then Request/Ack.

     ```
     INFO (46110) - setup: station ea:ce:1a:6f:94:0b associated
     INFO (46921) - setup: dhcp 192.168.4.2 -> ea:ce:1a:6f:94:0b
     INFO (46958) - setup: dhcp 192.168.4.2 -> ea:ce:1a:6f:94:0b
     ```
   - ✅ the form over TCP 80. **Verified with a phone**, end to end: the page
     loaded, a hostname in the broker field was rejected with the fields still
     filled in, a corrected form saved, and the unit rebooted straight into
     `store: configured for ...`.

     Two things the plan did not anticipate, both found by using it:

     - **Three connections, not one.** A browser fetches `/favicon.ico` on a
       second connection and opens others it sends nothing on. With a single
       socket the next real request is refused with a RST, so pressing **Save**
       gave "this site can't be reached" seconds after the GET that drew the
       form had worked. `CONNECTIONS = 3`, each with its own buffers, sharing
       the `Store` behind a mutex.
     - **Phone keyboards add a trailing space.** An SSID stored as `"fdlgrm "`
       then fails forever as `NoAccessPointFound`, which names neither the
       space nor the field. The inputs now set `autocapitalize=off
       autocorrect=off spellcheck=false`, and `provisioning::trimmed` strips
       whitespace from the SSID, host, port and username — but **not** from
       either password, where a trailing space may be real and trimming would
       make a correct credential impossible to enter.

       ✅ Both the bug and the fix observed on hardware, with the same phone:
       the same SSID that stored as `"fdlgrm "` now stores clean, associates,
       and the unit reaches `led: Healthy`.

     `mqtt_host` is validated as a literal IPv4 address at the form, because
     `mqtt.rs` has no resolver: a hostname would be stored, survive the reboot,
     and leave the unit retrying a connection it can never make.
   - ✅ the reset button on GPIO3, as a **boot** gesture rather than a runtime
     one — see *The outside button*. Now a true reset: with the fallback gone,
     erasing drops the unit into setup mode rather than being silently refilled
   - ✅ `examples/mkrecord.rs` + `dev/provision.sh`, writing the record from the
     host. Verified on the dev kit: provisioned, reflashed, still configured
     from flash. See *Credentials: getting them out of the binary*
   - ✅ `FDR2`: the record now also carries the two per-unit mechanical figures,
     so one binary can drive three different mechanisms. Stored and round-tripped;
     `feeder.rs` does not consume them yet, which needs the bench measurements
     A missing `nvs` partition is now a loud error rather than a quiet
     fallback: with no credentials to fall back on, such a unit cannot be
     configured by either route, and setup mode would be a lie because it could
     not save what it was given.

     `cfg.toml` keeps its credential lines, but only as input to
     `dev/provision.sh` — they never reach a compiler.
   - ⬜ **the panel in setup mode**: the SSID, the password and the address, on
     the glass instead of only on a console. Written and host-tested — the pure
     layer already laid it out, and what was missing was the wiring, because
     `setup::run` never returns and so could not spawn `display_task` itself.
     **Not yet seen on a panel.** It needs a unit with no record, which is a
     button held through power-on and therefore a capture nobody can automate:

     ```sh
     # hold the outside button, then start this; it resets on attach
     ./dev/capture.sh --seconds 40
     ```

     Look for `store: erased by the boot button`, then `setup: raising
     cat-feeder-<id>` and a `display: |...|` block spelling the same SSID.

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
      that distinction is made.
      **Confirmed by eye on a Zero as well as the dev kit**, in the order the
      console announces, so the two boards agree on channel order and
      `wire_word` stays one function
    - ⬜ tune the palette once a unit is in a kitchen. The constants in
      `indicator.rs` are dim on purpose but were picked by eye, and green reads
      much brighter than blue at the same number
    - ⬜ an external WS2812 on the Zero's GPIO8 pad, in parallel with the
      onboard one. Needs no firmware change — see *The RGB LED*.

      **Less obviously needed now.** The argument was that the onboard LED is
      sealed inside a feeder you cannot modify; with the board in its own
      printed case, the case can simply have a window or a light pipe over the
      module's own LED. Keep the option — it is still free, and a second LED
      placed where the feeder is rather than where the box is may yet earn its
      keep — but it is no longer the only way to see the thing
    - ✅ `Health::setup`. `main.rs` sets the flag on the way into setup mode
      and `Bus::health()` reads it, so the LED's blue flash comes from the same
      fact the boot path acted on rather than from a constant

11. Move the broker and Home Assistant to the Raspberry Pi 5. **The Pi is up at
    192.168.68.126**, both containers running from `~/ha`, which is a working
    tree of `github.com/filosganga/home-assistant`. The feeders still talk to
    the Mac's Docker stack — a laptop that is not always on.

    Note the Pi is reached by **address, not by name**. `ha.local` exists only
    over mDNS, and that is unreliable here: the Deco mesh reflects multicast
    between its nodes and lets it go stale, so a lookup that worked ten minutes
    ago fails now while the host is perfectly reachable by IP. Chrome never
    resolves it at all — Secure DNS hands `.local` to the upstream resolver
    rather than the LAN, so the browser gets NXDOMAIN every time regardless of
    the mesh. An `/etc/hosts` entry on the Mac fixes both at once.

    `ha/config` is also root-owned: the config edits below need `sudo` on the
    Pi and belong in that repo rather than being dropped on the box.
    - ✅ Mosquitto, and it is configured correctly: `listener 1883`,
      `allow_anonymous false`, a password file, and — checked, because it is
      easy to omit — `persistence true`. Anonymous connections are refused, as
      they should be
    - ⬜ **a `feeder` user in that password file.** The one there is Home
      Assistant's own. A unit provisioned with the dev credentials is refused,
      so this blocks repointing even once everything else works
    - ⬜ **onboard Home Assistant.** The container has run since first boot but
      nothing has been set up in it: `/api/onboarding` reports `user`,
      `core_config`, `analytics` and `integration` all `false`, and
      `core.config_entries` holds only what was auto-discovered — no MQTT.

      `core_config` is the step that sets the timezone, so **until it is done
      Home Assistant is on UTC**, which is exactly the silent hour-shift
      described above. Set `Europe/Rome` while onboarding rather than fixing it
      afterwards.
    - ⬜ add the MQTT integration, pointing at **`localhost:1883`** — Home
      Assistant runs with `network_mode: host` there, so the `mosquitto`
      container name that works on the Mac does not exist on the Pi. The
      package publishes through this integration, so without it every
      automation in it fails at runtime while the package itself loads cleanly
    - ⬜ **install `homeassistant/packages/cat_feeder.yaml` on the Pi**,
      unchanged — it is tracked here precisely so it can be.

      This one is load-bearing rather than housekeeping, and the failure it
      causes is the nastiest kind. That package is the half of the system that
      publishes `feeder/time` every minute. A feeder pointed at a broker where
      nobody publishes it takes the *retained* time, starts its clock on it, and
      then never arms the schedule — see *A retained `time` is not a trusted
      one*. The unit sits `online`, flashing red ×3, and does not feed. That is
      exactly correct behaviour and it is indistinguishable from a bug.

      So it comes **before** repointing any feeder, and it is checkable with no
      feeder involved at all:

      ```sh
      ./dev/watch.sh --host <pi> 'feeder/time'
      ```

      A line a minute means the Pi's half is done. Silence means Home Assistant
      is up but the package is not loaded.
    - ⬜ repoint the feeders. **This is a re-provision, not a rebuild**:
      `mqtt_host` lives in each unit's flash record, so it is
      `./dev/provision.sh` once per unit with the Pi's address, and no compile
    - ✅ the Pi's address is fixed first: **192.168.68.126**, reserved in the
      Deco against `98:fe:54:29:a4:a2`. It had to come before provisioning any
      unit, because there is no resolver in the firmware — `mqtt.rs` parses
      `mqtt_host` with `Ipv4Addr::from_str` — so an address that moves takes
      all three feeders off the air with no way back but re-provisioning each
      one
    - ⬜ decide what happens to the Mac stack. Keeping it is fine; two brokers
      with the same retained topics are not, so a feeder should point at one or
      the other, never be moved back and forth casually

### What is waiting on what

| Step | Waiting for |
|---|---|
| 3, the detent interval | **nothing — the bridge is wired and driving**; it needs the motor on a real mechanism |
| 6, flashing the three Zeros | **nothing — the boards have arrived**, jumpers to be soldered |
| 8, retiring the PCBs | 3, the third feeder being opened, **and an enclosure designed and printed** |
| 11, the Pi | nothing; both containers run. Home Assistant is not onboarded yet |
| a display | **nothing for development** — a 1.3" part is on the bench and is now the likely production part; the 0.91" ones are the fallback |
| the enclosure | v1.5 being settled, since the panel and any knob are most of what it holds |

**Every part is now on the bench**: the Pi, the three Zeros, the DRV8833 and a
display to develop against. Nothing in this project is waiting on the post any
more.

The work left is soldering and **CAD**. That second half is new: since the
electronics moved into their own printed case, step 8 cannot happen until
something exists to put them in — see *The electronics live in their own case*.
One design serves all three feeders, which is the whole point of it, but it is
one design that does not exist yet.

The first Zero is on a breadboard with the driver, the switch, the button and
the display, and it boots, sweeps its LED, answers both buttons, drives the
bridge and draws on the panel. `main.rs` builds a real `Drv8833` from the
`board.rs` pins, and a feed request has been watched turning a motor with the
yellow bench button standing in for the hub switch — align, count, brake.

What is left of step 3 is therefore the **measurement**, not the driver: the
detent interval wants the motor turning a feeder's actual mechanism, because
that is what sets the speed, and a bare shaft on a breadboard does not.

One wiring lesson from that first board, because it cost an hour and will
recur on the other two: **both buttons were wired with their GPIO and ground
legs in the same row group**, which grounds the pin and bypasses the switch
entirely. It presents as a unit that erases its own configuration on every
boot, which reads as a flash fault rather than a wiring one. The console now
names the level of both pins at boot — `currently pressed` on an untouched
button is the whole diagnosis — and pulling the jumper is the confirming test:
a floating pin with the internal pull-up must read `released`.

When wiring the next one, watch these in order. The power-on sweep must show
red, then green, then blue — both boards are RGB, so a swap now means that
board's WS2812 differs and `led::wire_word` becomes board-dependent. Then
**check the motor's direction before bolting anything to a feeder**. The
bridge itself is proven now, but proven on a bare motor: which way `IN1=1,
IN2=0` turns a hub that has a mechanism bolted to it is still unobserved, and
finding out afterwards means taking it apart again.

**Next, and written up above with enough detail to start cold:** the lost DHCP
DISCOVER, ten seconds of pure waiting on a ten-second timer. It is now the
single largest thing in a start-up, `feeder/time/request` having taken the
other forty-nine seconds out.

Later (not now): a short press on the GPIO3 button feeding one portion, so a
manual feed works with the broker down; battery backup.

**A display. The part is ordered.** The original LCD window is 40 × 18 mm,
which pointed at a 0.91" 128×32 I²C OLED — roughly a 38 × 12 mm module, two
pins, a 512-byte framebuffer, and **three** lines of 21 characters. Five of
them are on the way (SSD1306, I²C, `GND · VCC · SCL · SDA`). The common 0.96"
128×64 was the wrong shape for that window: its module is near enough square at
27 mm tall and would not go in.

⚠️ **That window is no longer the constraint** — see *The electronics live in
their own case*. A printed enclosure has no window to match, so the **1.3"
128×64 already on the bench can be the production part**, and the sizing
argument above is kept as the reasoning that was true while the original shell
was, not as a live requirement. The paragraphs below still hold; read "must
fit" as "must at least fit".

**Develop against the 1.3" regardless.** The driver crate and the two wires are
identical and only a size parameter differs — `oled.rs` swaps `PanelSize` on
the `panel-128x64` feature and nothing else moves.

**Three lines becomes a floor. Twenty-one columns does not.** Both panels are
128 pixels wide, so `FONT_6X10` gives 21 characters on either and
`display::COLS` is unchanged at 21; only `ROWS` grows, from three to six. Lay
every screen out to work in three lines, because the 0.91" parts are the
fallback and a layout built for six cannot be shrunk into three — extra rows
are somewhere to put more, never somewhere a required line may hide. But treat
21 as the hard limit it has always been: `Line` is `String<COLS>` and `push`
truncates silently, so an over-long line is lost on the glass with nothing said
on the console.

That figure was *two* here until the setup screen needed its third line, and it
was simply wrong rather than conservative: `FONT_6X10` is ten pixels tall, so a
32-pixel panel takes three rows with two pixels spare. `display.rs` has said
`ROWS = 3` since it was written, and the third line is the one the setup screen
puts the address on — the whole reason it beats a serial console. Anyone laying
out against "two" would drop exactly that.

⚠️ **Check which controller that 1.3" module actually has before blaming any
code.** Many 1.3" 128×64 boards are **SH1106**, not SSD1306: it has 132 columns
of RAM with the panel wired to the middle 128, so an SSD1306 driver renders
everything displaced two pixels with the edges wrapped. It looks like a broken
framebuffer and is not one. The 0.91" parts are genuine SSD1306.

Pins are not a constraint — I²C routes through the C6's GPIO matrix, so any free
pair works. **Assigned: `SDA` on GPIO18, `SCL` on GPIO19**, both in `board.rs`
with every other pin. They are edge castellations rather than the equally free
GP6/GP7 back pads, which matters only because the board is hand-soldered.

What sells it is setup mode, and **that is now wired**: a unit raising its own
network shows what to join and what to type into it, which is the one screen
whose contents exist nowhere else. Without it the password of the network the
unit just raised is only reachable over a serial cable, which is the whole
reason for the salted derivation, `dev/ap-password.sh` and printing stickers
before first power-on.

```text
cat-feeder-99177c
H75T-C7VT-6FAV
http://192.168.4.1
```

`main.rs` spawns `display_task` **before** calling `setup::run`, because that
function never returns and could not spawn anything afterwards. The panel is
brought up before the boot decision for the same reason: both outcomes want a
screen and only one of them can come back for it.

Three lines, three facts, and the third is the address — which is now one
constant. It used to be written twice, as an `Ipv4Addr` in `setup.rs` and as a
string in `display.rs` with a comment asking them to agree. `provisioning.rs`
holds `AP_ADDR_OCTETS` beside the SSID and password derivations, the socket is
built from it, and a host test pins the printed URL against it. A panel
confidently showing an address nothing answers on would be worse than no panel.

The salt is still needed — it is what stops a stranger deriving the password
from the MAC in the beacon — and the sticker drops from required to backup **the
day a capture shows those three lines on a panel**, not before. Until then it is
the only thing that works, which is why `README.md` still tells a reader to
print one. A unit whose screen turns out blank on the production part, with no
sticker and no serial cable, cannot be joined at all.

It does not make the LED redundant: at 0.91" you read a screen standing at the
feeder, while the LED answers *is anything wrong* from the doorway. Burn-in over years of showing `next 08:00` is
handled by sleeping and waking on a press, and the collision that used to imply
— a press that both wakes the screen and feeds — is resolved by the gesture
table above: a tap only feeds while armed. Night glare is the same blanking. The
split stays what it is everywhere else in this codebase: a pure layer deciding
*what to show*, host-tested, and a gated task that pushes pixels.

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

### Version 1.5: the knob

**Decided in principle, not built.** It sits between the working prototype and
the printed case: after the mechanism is proven and before the enclosure is
drawn, because a panel and a knob are most of what an enclosure is *for* and
designing one around a bare board twice is the wasteful order.

What is done today is reserving two pins in `board.rs` and half-reserving a
third against the fork below.

**There is no deadline on it**, which there would have been under the original
plan of reusing each feeder's own window: a hole drilled in a commercial shell
cannot be undrilled, so the decision would have expired at step 8. *The
electronics live in their own case* removes that — a printed part is
reprintable, so the interface can keep moving after all three feeders are
closed up. The pins are reserved in `board.rs` because a *pin* spent elsewhere
is the one thing a reprint would not recover.

**The argument.** *A second version* below asks what a person who did not build
this has to install before the feeder works, and answers it with the unit's own
entities and its own admin page. Read those back and the answer is still *join
it to a network first*: the discovery entities need Home Assistant, the admin
page needs the unit on the house Wi-Fi, and even setup mode needs a radio, a
phone and someone who can read a password off the panel. **Every editing route
v2 proposes is a networked one.** A knob and a panel are the only pair that is
not, which is why they come first.

**And it reaches further than the schedule.** A knob can enter text —
character by character, the way a Prusa's menu does — which means an SSID and a
Wi-Fi password can be typed on the device.

Every route into a unit's credentials today needs **a second machine**: a phone
with a browser for setup mode, or a laptop with `espflash` and a USB cable for
`dev/provision.sh`. Neither is a hardship here, where both are on the desk — but
they are the reason a feeder cannot be reconfigured by the person standing in
front of it, and *that* is what a knob removes. Tedious as a primary route and
nobody's first choice. It does not retire setup mode, which is faster and
already works; it ends setup mode's monopoly on the case where the phone is
what you do not have.

With the display carrying it, the 1.3" 128×64 can be the production part and a
menu has **six rows** to work with rather than three. It still has only
twenty-one columns — both panels are 128 pixels wide — so a menu item's text is
as tight as it ever was, and a knob menu is exactly the feature most likely to
forget that. See *A display*.

A rotary encoder — an EC11, quadrature `A`/`B` plus a push switch in the shaft —
and the panel answer it with **nothing**. That is the argument, and it is the
whole argument. It is *not* nicer page-stepping: the locked-tap cycle in *The
screen's pages* is designed but not built, and when it is built it will be good
enough that turning a knob instead would not be worth a hole in a case.

**It removes SNTP rather than adding to it**, which is the opposite of what a
second input device usually does. A knob-set clock is set in local wall-clock
time, the schedule slots are already local wall-clock time, so nothing converts
and no timezone rules go on the device. DST is someone turning a knob twice a
year, exactly like every oven in the house. `feeder/time` stays as a convenience
for units that have a network, and the *configured feeder timezone* note stays
what it is: worth doing only if the broker ever publishes UTC.

**The trust ladder gains a rung and keeps its floor.** Today there are three
states, not two — see *A retained `time` is not a trusted one*: a **live**
`feeder/time` arms the schedule, a **retained** one starts the clock but leaves
it holding, and nothing at all means the unit waits. A clock of its own inserts
a rung, and *power-cycled and no broker → wait, never guess* survives intact
because the bottom rung is still waiting.

**Where the RTC sits against a retained time is the open question**, and it is
the one to settle before writing any of this. They are rivals for the same rung:
a retained time proves Home Assistant published *at some point*, an RTC-backed
clock proves a human set it *at some point*, and neither carries its own age.
The argument for the RTC winning is that a hand-set clock with a live
oscillator has been running continuously since it was set, whereas a retained
message is a snapshot of unknown vintage — which is precisely the distinction
that section already draws, applied one level down.

**The rule this would overturn is narrower than it first looks.** *Once armed,
retained times are ignored outright* is not at stake: `LocalClock::align` already
drops a retained time whenever `self.trusted`, with
`a_retained_time_after_trust_is_ignored_rather_than_applied` pinning it, so a
reconnect cannot drag a trusted clock anywhere and an RTC costs nothing there.
The rivalry is entirely in the **untrusted** window, where a retained time
currently does start the clock — the `clock: started, … (retained; waiting for a
live time)` line. That is the one rule to argue about, and whoever implements
this should look there rather than in `LocalClock::align`'s trusted branch.
(`Scheduler` is the other struct in that file and has no `align` — it owns
`consumed_through` and `next_due`.)

#### Which means an RTC, and it costs no pins

The C6 has no battery-backed clock. Its low-power timer runs from the chip's own
supply, and a feeder takes 5 V from the original USB port, so a power cut takes
the time with it. A knob-set clock that dies at the next outage answers the
question in the room and not the one on holiday.

So a **DS3231** with a coin cell, on the I²C bus **already wired for the
panel** — GPIO18 `SDA`, GPIO19 `SCL`, no new pins, about €2. It answers on
`0x68`, the SSD1306 on `0x3C`/`0x3D`, so `oled.rs`'s probe is unaffected. A
PCF8563 is the cheaper alternative and is at `0x51`, not `0x68` — worth knowing
before scanning a bus for a part that is not there.

**Pick the DS3231 for its oscillator-stop flag, not for its accuracy.** The
middle rung of that ladder is only legitimate if it can say *I was never set*:
a cleared RTC otherwise reads as a plausible date rather than as an absence, and
a plausible date is exactly what *never guess* exists to refuse. `OSF` is set
whenever the oscillator has been without power, so "coin cell flat, or never
fitted" is a fact the firmware can read rather than infer. The ±2 ppm is a
bonus; the flag is the reason.

⚠️ **Most DS3231 breakouts carry a charging circuit for a rechargeable
LIR2032.** Fitting an ordinary CR2032 to an unmodified module tries to charge a
primary cell. The fix is lifting the series resistor or the diode. It presents
as a battery flat in months, which reads as a bad module rather than as a wiring
decision.

⚠️ **Two I²C modules each with their own pull-ups** put those resistors in
parallel and stiffen the bus. Two at 400 kHz is normally fine; it is the first
thing to check if the panel starts misbehaving only after the RTC goes on.

#### What it costs

**A knob puts pressure on the one cat defence that cannot be got round, and
that is the thing to settle first.** *The outside button* says to **recess the
button**, because needing a fingertip defeats a paw outright and mechanical
protection survives any sequence of lucky presses; everything else is what
`button.rs` calls "the second line of defence, not the first". A knob has to
protrude to be turned, and a recessed knob is not a thing.

**The printed case takes most of the sting out of this**, and it is worth
saying before the fork rather than after. A control panel in its own box can be
mounted high, or behind the feeder, or anywhere a cat has no footing — which a
panel set into the original shell's window never could be, because that window
is wherever the manufacturer put it. Placement is a defence the old plan did
not have, and it is available to both rows below.

What remains depends on **whether the knob is also the feed button**, which is
a fork in the hardware:

| | Pins | What it costs |
|---|---|---|
| **The knob replaces the button.** Its shaft switch takes GPIO3 | 9 of 13 | one control, simplest wiring — but it protrudes where the button was recessed, and the recess defence is gone |
| **The knob is a second control, separately placed.** The button stays on GPIO3 exactly as today; the encoder gets `A`, `B` and its own switch | 10 of 13 | one more pin and one more part — and the two can then go in different places, the button where a human reaches in a hurry and the knob where a cat does not |

Both fractions count from the seven spent today — GP0–GP3, GP14, GP18, GP19,
with six free. *Which pins are usable* above says why that is seven and not the
eight pins this design wires.

**The second is the better design, and the reason is placement rather than
paranoia.** A configuration knob does not have to be reachable in a hurry; the
feed button does, because arm-then-tap is the one feeding path that works with
the broker down. One control cannot be both out of a cat's way and to hand, so
reusing GPIO3 forces a single compromise position — and takes the boot-erase
gesture there too. Splitting them keeps the recessed button exactly as it is,
verified and unchanged, and makes the knob **physically incapable of
dispensing**, which is a stronger guarantee than any amount of gesture logic.
It also shrinks the menu hazard below to nothing, because a cat reaching the
menu at all stops being a scenario.

It still fits: `A`/`B`/`SW` on GP20/GP21/GP22 leaves GP6, GP7 and GP23 free.
`board.rs` reserves GP20/GP21 today and says which count applies to which fork.

**A cat in a menu is a hazard class this design does not have yet**, and it is
the reason the first fork above needs all of what follows while the second
mostly does not. Every gesture in a menu is foodless, which is the property the
arming design rests on — but the *consequences* are not. A cat that leaves all
eight slots set to sixteen portions, **at eight different times of day**, empties
the hopper without one foodless rule having been broken.

The "different times" is load-bearing and is the part easy to get wrong:
`schedule.rs`'s consumed marker is keyed on
`(day, minute-of-day)`, so eight slots sharing one minute resolve once and
`MAX_CLICKS` caps that at sixteen clicks, exactly as it should. Spread them
across the day and there is no guard left, because capping a day was never
`MAX_CLICKS`'s job. So menu entry is a hold, the menu times out back to the home
screen, and an edit commits on an explicit confirm rather than as the knob
turns.

**Under fork (a), a hold then means two things by context** — arm-to-feed on the
home screen, back-or-save in a menu — which is more vocabulary than `button.rs`
has today and the part most likely to read as a button that does nothing. Fork
(b) does not pay this at all: the GPIO3 button keeps exactly today's four
gestures, and back-or-save lives on the knob's own switch.

**Rotation stays foodless, always**, and what that means also depends on the
fork. Under (b) it is trivially true, because the knob is behind the lid and can
reach nothing but menus — page-stepping is left to a locked tap on the front
button, which is where *The screen's pages* already designs it and where the
person reading the panel actually is. Under (a) the knob is the only control, so turning steps
pages while locked and could set the portion count while armed, with only a tap
ever dispensing.

⚠️ **That last part — "sets the portion count while armed" — overturns a
decision, and should not be smuggled in as an improvement.** It is a fork-(a)
temptation specifically, and one more reason (b) is the cleaner design. *Manual feeds accumulate* says there is
no default portion size — every feed path states its own count, the button as
`1` — and both buttons implement that deliberately: `mqtt.rs` pins
`payload_press` at 1 ("three portions is three presses"), and `button.rs` has a
test named `three_portions_is_three_taps_not_three_arms`. A count held between
taps *is* a default portion size, with all the state that implies: what it
resets to, whether it survives the window lapsing, and what the panel shows when
it disagrees with what the next tap will do. That may well be worth it — three
taps for three portions is tedious — but it is a reversal to argue for, not a
wart to fix in passing.

**The panel becomes load-bearing.** Today a blank screen is a degraded unit.
With knob configuration it is an unconfigurable one — the same fear *A display*
already raises about setup mode, generalised to everything.

**It does not replace the admin page**, which is still the pleasant way to do
this from a sofa. What it changes is the dependency graph: with a knob, Wi-Fi is
optional rather than required, and a feeder that works with no network at all is
a different thing from one that degrades to not feeding.

### A second version: the unit owns its clock and its schedule

**Decided in principle, not started, and not to be smuggled in one commit at a
time.** It contradicts *No local RTC, no NTP, no flash persistence* above, and
it is meant to: that rule is right for a system whose only user owns the broker,
and wrong for a feeder somebody else is given. What follows is one decision with
seven consequences, not seven options.

The question that forces it: what does a person who did not build this have to
install before the feeder works? Today the answer is a YAML package in Home
Assistant, and without it the unit connects, publishes discovery, shows a Feed
button that works — and never feeds, sitting `online` flashing red ×3. That is
correct behaviour and indistinguishable from a fault. Every comparable product —
PetLibro, SureFeed, Aqara, Shelly, Tasmota, an ESPHome device — answers it the
same way: **the device owns its configuration and the app is a control surface**.

**1. The unit owns its schedule, and a new unit starts blank.** Being given a
schedule is an explicit act, not something a unit inherits by connecting. The
present design has the opposite property — `feeder/schedule` is one shared
retained topic, so a unit that joins is immediately feeding meals nobody chose
for it. That is a hazard *today*: a board on the bench pointed at the house
broker will start turning. A blank unit fails toward not feeding, which is the
direction this project chooses everywhere else — *a missed meal is preferable to
a double one*, and *power-cycled and no broker → wait, never guess*.

Blank is only defensible because (5) gives it somewhere to be filled in that is
not Home Assistant: the first schedule is typed into the feeder's own page, or
pushed to it deliberately. Without that page, "starts blank" would mean "needs
Home Assistant before it can feed at all", which is the dependency this whole
section exists to remove.

**2. Which forces flash persistence.** If the schedule lives only in retained
per-unit topics, the broker is the unit's memory, and a wiped broker — `down -v`,
a migration to another host, a Mosquitto without `persistence true` — silently
blanks every feeder with nothing left to republish it. Today that recovers by
itself because Home Assistant republishes on restart; with Home Assistant out of
the loop, nothing does. So the schedule goes in the record in `nvs`, beside the
credentials and the per-unit timings that are already there. Device-owned
schedules and flash persistence are the same decision.

**3. Which forces the clock.** A unit that keeps its own schedule and waits for
someone to tell it the time has moved the dependency rather than removed it. So
SNTP, and with it real timezone rules on the device: the *configured feeder
timezone* note above stops being optional, because SNTP gives UTC and nothing
else. That weight is exactly what today's design avoids by assuming the broker
shares a timezone, and it is the price of the unit standing alone.

**Amended by *Version 1.5: the knob* above**, which is the cheaper answer: a clock set by
hand is set in local wall-clock time, so it needs neither SNTP nor timezone
rules. Read this point as *which forces the unit to own its clock somehow* —
SNTP is one way and the knob is another, and the knob is the one that also works
with no network at all.

**4. The editor is the unit's own entities, so nothing is installed.** MQTT
discovery has the platforms for it — `time`, `number`, `select`, `text` and
`datetime` all exist and all build on `MQTT_RW_SCHEMA`, which is
`command_topic` + `retain` + `state_topic`: the same shape as the `paused`
switch that already works here. Eight `time` plus eight `number` entities in the
unit's own device block means a feeding time is a time picker on the feeder's
page in Home Assistant, with no package, no helpers and no YAML. (Checked
against the Home Assistant in `compose.yaml`, 2026.9.2.) The unit's own web
server is the other surface, since `setup.rs` already serves a form — which is
what Tasmota and Shelly do.

**5. The unit serves its own admin page, in station mode and not only during
setup.** Most of it is already built: `setup.rs` parses a request line and a
`Content-Length`, renders a form and parses it back, and runs three connections
with their own buffers, and a configured unit already has a network stack. What
is new is running the listener alongside MQTT, authenticating it, and a page for
the schedule.

It is also the re-provisioning route the button was always a poor substitute
for. *The value is in re-provisioning, not first boot* is already written above,
and "hold the button through power-on, then set it up again from a phone" is a
heavy price for a Wi-Fi password that changed under three units screwed into
place. It adds no boot state: the boot decision stays *no valid record → setup
mode*, and the admin page is one more writer of the same record, so *One way in,
not two* still holds.

Four rules it comes with:

- **The admin password is derived by default**, `base32(sha256("<ap_secret>:<id>"))`
  — the same string the sticker carries and `./dev/ap-password.sh` prints. A unit
  is then never unauthenticated, even if the field is left blank during setup,
  and recovery needs no password-reset flow because the boot gesture erases the
  record. The physical button stays the root of trust, which is the right answer
  for a device with no other identity.
- **Never render a stored secret back into a form.** The setup page re-fills
  fields on error, and that must not extend to passwords once the page is
  reachable from the house network. Mutations are POST only. It is plaintext
  HTTP on the LAN, like every comparable device — worth stating rather than
  implying otherwise.
- **`configuration_url` in the discovery `device` block** (abbreviated `cu`, and
  accepted by the schema — checked) gives Home Assistant a *Visit device* link
  at whatever address the unit had when it connected, republished on every
  reconnect. mDNS is not an option here, for the reasons the Pi's entry gives
  about `.local` on the Deco mesh. The display is the other backstop, but **it
  does not show the address yet**: `display::render` puts one on screen only in
  setup mode, and the station screen spends its three lines on the status
  banner, the last feed and the next one. The gesture is not the missing part —
  any press already wakes the panel — the content is, and it is **this version's
  work rather than v2's**: see *The screen's pages* below.
- **Saving Wi-Fi or the broker reboots; saving a schedule must not.** A feeder
  that restarts when a mealtime is adjusted drops its clock trust and goes back
  to waiting for a live time.

The cost is RAM and a permanent listener. Three connections exist because a
browser opens several at once, and those buffers would now coexist with MQTT,
the feeder, the schedule task, the indicator and the display — that wants
measuring on a Zero rather than estimating. An always-on HTTP server is also
permanent attack surface on the house network, which is the real reason the
authentication above is not optional.

**6. Synchronising three feeders is an explicit broadcast.** `feeder/all/schedule`,
**not retained**, applied by each unit and echoed into its own per-unit retained
state. No retained topic races another, because only the per-unit topic is state
— the same command/state split that already separates `feeder/all/feed` from
`feeder/<id>/paused`. It is also the right shape for the decision in (1): one
deliberate gesture saying *these three eat the same meals*, rather than
inheritance by accident.

**Subsets are a Home Assistant label, not a firmware feature.** A label is an
arbitrary tag put on devices in the interface, and `label_devices('<name>')`
returns their device ids — from which `device_attr(d, 'identifiers')` gives
`feeder_<id>` directly, so a labelled subset is the same template as the one
pause already uses with `model` swapped for the label, and one hop shorter.
`area_devices` does the same by room. That covers *feed only the two upstairs*
with **no new topic and no firmware change**, because Home Assistant never
actually broadcasts: it publishes to each unit's own topic, and a label only
changes which units it loops over. Areas and labels are both in the version in
`compose.yaml`.

A group *in the firmware* — `feeder/group/<name>/schedule`, each unit
subscribing to its own — is only needed when the sync happens with no Home
Assistant in it, from the unit's web page or a bare `mosquitto_pub`, because
then nothing is there to expand a list. It is not worth building before that,
and it costs three rules if it ever is: `feeder/all/*` must keep meaning
everyone regardless of group, so a misconfigured unit stays reachable; groups
must apply to `feed` as well as `schedule`, or *all* quietly means two different
things on two topics; and membership is per-unit config that can be silently
wrong, so a unit with a typo'd or unset group never receives a broadcast and
looks perfectly healthy — the same silent failure as (7), and it belongs in the
state payload and on the console for the same reason.

**7. Blank has to be visible.** Once "no schedule yet" is a legitimate state, a
unit that is online, connected and clock-trusted with no meals configured looks
exactly like a working feeder: dark LED, nothing wrong, never feeds. That is the
same silent failure as a feeder left paused, which is already the one state where
nothing alarms. It needs its own indication — an LED code, or a line on the
display, which is what the display is best at.

**What dies with it.** The shared retained `feeder/schedule`, and with it the
property that a replacement unit comes back already knowing the house's meals.
That loss is the point of (1), but it is a real loss and it should be a
deliberate one. Home Assistant also stops being able to see the schedule as one
fact; it sees three units' worth of entities.

**What Home Assistant is left doing**, and it is worth having: the button, the
pause switch, the jam sensor, history, and *optionally* a "copy this feeder's
schedule to those feeders" automation. **That is where a blueprint finally
fits** — one automation, no helpers to create, and, crucially, not load-bearing:
a recipient who never imports it sets three schedules on the feeders' own pages
and they still feed. The present package is the opposite, which is why shipping *it* as a
blueprint was the wrong idea. Blueprints cannot define helpers or bundle three
automations, so as long as Home Assistant owns the schedule, the package stays.

**Until then, today's design stands**, and the near-term answer to *how does a
person change the feeding times* is `input_datetime` and `input_number` helpers
defined by the package — eight slots, so the UI cannot express a schedule the
firmware would reject — with `meals` templated from them instead of being a YAML
literal. That is an afternoon's work in one file and no firmware change, and it
is worth doing only if schedule editing has to be user-facing before v2.

Pausing already finds its units rather than being told them — see *Pause stops
the schedule, not the feeder*. That one was cheap enough to do immediately, and
it is what any of these futures wants anyway.

### The screen's pages

**This version, not v2.** The panel and the button both exist; what is missing
is that the unit cannot tell you its own address, or anything else about how it
is configured, without a serial cable.

The wake half is already built. `main.rs` records `BUS.last_press` on the
**press** edge rather than the release, deliberately, so the panel is lit before
a finger lifts, and `display::awake` reads that timestamp. What is missing is
content: `display::render` puts an address on screen only in setup mode, and the
station screen spends its three lines on the status banner, the last feed and
the next one.

A locked tap steps through pages, per the gesture table above:

| Page | Lines |
|---|---|
| 1 | status banner, last feed, next feed — today's screen, unchanged |
| 2 | the unit's address, and the SSID it is on |
| 3 | the broker: host, port, username |
| 4 | device id, firmware version, and what this unit was calibrated for |

Page one is the existing screen rather than *last feed* and *next feed* being
two pages of their own: three lines already show both at once, so splitting them
would cost a tap to see something that was never hidden.

Page four is worth its place because of something already true of the console:
*a feeder behaving oddly is either mis-measured or mis-provisioned and nothing
else tells them apart*, and today the calibration is printed once at boot and
then only over USB. On the panel it is readable at the feeder, which is where
somebody stands when the portions look wrong.

Four rules:

- **No page shows a password**, ever. Setup mode is not an exception to this so
  much as a different thing: the password it shows is one the unit generated for
  a network it raised itself, and showing it is the entire point. A stored Wi-Fi
  or broker password is never rendered, on the panel or in a form — the same
  rule the admin page will need.
- **Twenty-one columns is the budget**, so lines are built against `COLS` like
  every other line in `display.rs`. `192.168.68.114` is fourteen characters and
  fits; `http://192.168.68.114` is exactly twenty-one and would fit only for
  addresses that short, so print the address bare and let whoever reads it type
  the scheme.
- **Sleep resets to page one**, as the button section says, so the first press
  never lands on a page left over from days ago.
- **Pages are a locked gesture only.** While armed a tap feeds, and a screen
  control that dispenses food is exactly what arming exists to prevent.

Keep the list short. A screen with a menu is a screen nobody reads to the end,
and everything here is either the feeder's purpose or an answer to *how do I
reach this unit* — which is the question the admin page in (5) makes routine.

The split is unchanged: `button.rs` gains a lock gesture and a page-step event,
`display.rs` gains a page to render, both pure and host-tested, and the task
still only pushes pixels.
