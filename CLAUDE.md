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
| 220 µF 16 V electrolytic | across the rail just after the diode-OR (brown-out on motor start); its two legs are the star points for 5 V and ground — see *The perfboard* |
| 5 V from the feeder's original USB port | ≥1 A adapter |
| 3×AA in the feeder's own compartment | **backup only**, joined through a diode-OR (MBRF2045CT) so nothing ever charges them — since 2026-09-28, see *The perfboard* |

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

⚠️ **The paragraph above predates the diode-OR on the perfboard, and no longer
holds there.** The adapter now reaches the rail through the MBRF2045CT, so it
arrives about a Schottky drop lower — no longer above `VBUS − Vf` — and the
laptop through `D1` and the adapter through the MBRF sit at about the same
voltage and **share the rail**, the motor's current included. Derived from the
two parts' drops, not measured. It changes nothing for a running motor; for a
jam test, unplug the laptop rather than trusting the adapter to carry the stall.
*Do not fit an external diode* above still stands: it is about the path between
the pad and the laptop, and the diode-OR is on the other side of the rail.

**Ground is shared, and must be.** The DRV8833's inputs and `nSLEEP` are
referenced to the Zero's ground, so the adapter's ground, the driver's and the
board's are one node. Sharing ground while the 5 V rails are separately sourced
reads as contradictory and is not.

The rail itself is one node with three taps — adapter, `VM`, and the Zero's
pad — so on the breadboard motor current flows adapter → rail → `VM` and never
crosses the pad. The pad is a load on that node, not a link in the path. (On
the perfboard the laptop can share it; see the note below.)

⚠️ **The one combination to avoid is provoking a jam on USB alone.** With no
adapter, the motor's current *is* drawn through `D1` and `P8` pin 1. That is
fine for a running motor at a couple of hundred milliamps — it is how every
bench test so far has run — but a stall held for the whole jam budget is more
than a 1 A Schottky should carry. Plug the adapter in before testing a jam —
and on the perfboard, unplug the laptop as well.

Both supplies live is also exactly what the **detent interval** measurement
needs: the motor turning a real mechanism on its real adapter, with the console
on USB.

### The third feeder is a different brand

Two of the three units are the same model. The third is a different brand,
similar-looking, and the mechanical figures in the table above were measured on
the matching pair only.

**Opened 2026-09-30: the same mechanism with cheaper parts.** Its hub sensor
is a **microswitch**, clicking **four times per full turn**, so the firmware's
shape holds unchanged. The motor is an **HC 180-15180, 5 V**, a 180-size motor
in its own reduction gearbox — a different part from the DRF-W500CA, same
voltage, same driver. **Neither brand fits a suppression capacitor** across the
motor terminals, and the matching pair has never shown a spurious click
without one; add 100 nF there only if this unit does. Its wiring leaves on a
**flat ribbon cable** instead of separate wires, in a different order — the
mapping is under the header table in *The perfboard*. **It is
much slower**: 5297 ms between clicks, measured by *Run calibration* on USB
with 4.7 V across the motor — so the driver is passing the full rail, and the
slowness is the mechanism. That was past the old 5000 ms ceiling, which is
why `provisioning::MAX_DETENT_MS` is now 10 000 and is the one place it lives.
Still to measure: the interval with a full hopper, and what one click
dispenses.

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
`feeder/all/schedule` sends the same `portions: 2` to every unit, so it
reaches each one identically, and a mechanism that dispenses a different
amount per click needs a per-unit scale. That is built: see *Per-unit portion
size*. What is needed from the bench is the ratio.

Both boards are the same chip; only GPIO numbers differ. Keep the pin map in
one place (`src/board.rs`) selected by a Cargo feature: `board-devkit`
(default) / `board-zero`.

### The electronics live in their own case

**Decided, not built.** The original plan was to reuse each feeder's own LCD
window and button hole. That is what *A display* below is still written against,
and it is why three lines of 21 characters appeared there as a hard constraint.

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
  so a 128×64 panel is the production part: a 0.96" **SSD1315**, on the bench
  and working. The 0.91" 128×32 fallback and the `panel-128x64` feature that
  selected between them are gone; `display::ROWS` is 6.

  ⚠️ **That bought rows, not columns.** Both panels are 128 pixels wide, so
  `FONT_6X10` gives **twenty-one characters**, and `display::COLS` stays 21.
  **21 columns is a hard ceiling**, and it is the half that bites: `Line` is
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

What it does **not** change: the third feeder still has to have its detent
interval measured. That is in the mechanism. (Its switch is confirmed a
microswitch — see *The third feeder is a different brand*.)

### Which pins are usable

**The Zero is the binding constraint, and it is tighter than the dev kit.** Its
pad map brings out GP0–GP9 and GP12–GP23, with GP16/GP17 appearing as `TX`/`RX`.
**GPIO10 and GPIO11 are not brought out at all**, on neither the edge
castellations nor the back pad row. Both were in the original pin map and both
have moved; see `src/board.rs`, which is still the one place any number lives.

Then subtract what is already spoken for:

| Pin | Why not |
|---|---|
| GPIO8, GPIO9, GPIO15 | strapping that matters: boot mode, boot log, JTAG source |
| GPIO12, GPIO13 | native USB D−/D+; on the Zero, the only console there is |
| GPIO8 | also the onboard WS2812, so already committed |

That leaves GP0–GP5, GP14 and GP18–GP22 on the edge, plus GP6, GP7 and GP23 on
the back pads — **fifteen usable, nine of them spent today**. **`board.rs`
carries the assignment table**, which is the thing to solder against; it is not
repeated here.

Nine and not ten: this design wires ten pins, but GPIO8 is not one of the
fifteen — it is struck out twice in the table above, as strapping and as the
onboard WS2812, so it was never available to spend. Count the free pins against
nine or the arithmetic comes out one short.

**GPIO4 and GPIO5 are strapping pins too, and the encoder's `A`/`B` are on them
anyway.** They are `MTMS`/`MTDI`, and on the C6 they only set the SDIO slave's
sampling edges; boot mode is GPIO8 and GPIO9. So an encoder holding them at any
level through reset cannot stop a unit booting. This list used to strike them
out with the other three, which was more cautious than the chip needs.

"No alternate function" was the rule that first picked GPIO10 and GPIO11. It
does not really apply here: on the C6 peripheral signals route through a GPIO
matrix, so the labels on a pinout diagram are a convention rather than a
restriction, and any pin outside that table will do.

GPIO8 is the onboard WS2812 on both boards. On the Zero it is *also* on the
back pad row, so an external WS2812 wired there sits in parallel on the same
data line and shows the same colour — an indicator outside a closed case for no
extra pin and no firmware change. See roadmap step 10.

### The perfboard

**`pcb.diy` is the only layout**, redrawn 2026-10-01 for a double-sided,
plated-through board and built as unit `99177c`, wired for the third feeder.
`9a6ecc` runs on the first layout (2026-09-28), which differs in its header
order, its driver channel and its hole names; that drawing is in git history
only (`git show 8920020:pcb.diy`), and nothing below describes it.

Verified on `99177c`: the Zero enumerating and booting headless, the LED sweep,
`switch: watching GPIO2`, the DS3231 answering and arming the schedule from
itself 1.4 s after power-on, and a calibration run and feeds through the
driver. On `9a6ecc`, the same design on the first layout, it also went **into
its feeder on batteries alone** and fed through the always-on broker with no
jam — so the slower motor on cells still clicks inside the jam budget.

It is the **headless** build — no panel, no encoder, GP3–GP5 unconnected —
with the DS3231 on the I²C bus as usual. **There is no external BOOT button**:
the Zero's own BOOT button is the only reset, so the case needs a pinhole over
it. A factory-fresh Zero arrives running Waveshare's demo, which leaves
ESP-IDF data in `nvs`; the schedule sector then reads `schedule: stored record
is unreadable` until a schedule is sent.

**The drawing is the component side**, with the bottom-side traces seen
through the board. Its columns run `A … X` left to right, as this board's
printed labels do from the component side, and rows 1–18 bottom to top. Every
hole below is named by those labels, which name the same hole from either
side — so count by label, never by "third from the left", which is exactly the
count that flips when the board is turned over. The first layout's board was
labelled the other way, `X … A`, which is one more reason its hole names do not
carry over.

**Modules sit in female headers**, not soldered, so a redesign costs the board
and a few parts rather than the Zero, the driver and the RTC. Socketed modules
can go in reversed or a row off: mark each socket's GND end on the board, and
beep with the modules in, from module pin to Zero pin.

**The feeder's cable** arrives on one 6-pin header along the bottom edge,
`F1 … A1`, in the **third feeder's ribbon order**, so its ribbon plugs
straight in. The matching pair's cables are in another order (5 V, Vbatt, GND,
switch, motor 1, motor 2) and need re-crimping to this one before either goes
on this layout.

| Pin | Hole | Signal | Goes to |
|---|---|---|---|
| 1 | F1 | GND | C1 −, the ground star point |
| 2 | E1 | Vcc, 5 V from the feeder's USB | MBRF2045CT, one anode |
| 3 | D1 | Vbatt, 4.5 V from the feeder's batteries | MBRF2045CT, the other anode |
| 4 | C1 | motor 1 | DRV8833 `Out1` |
| 5 | B1 | motor 2 | DRV8833 `Out2` |
| 6 | A1 | hub microswitch | **R1, 1 kΩ**, then GPIO2 |

The ribbon's conductors after the switch are the loudspeaker's and stay
unconnected. It has one switch conductor, so the switch's other leg is ground
inside the feeder — beep ribbon 6 ↔ 1 while turning the hub before fitting: it
must close once per detent. A switch returning to Vcc instead would put 5 V on
GPIO2 through R1.

The driver runs on channel A: GPIO0 → `In1`, GPIO1 → `In2`, GPIO14 → `Ult`,
`Out1`/`Out2` to the motor. The firmware does not care which channel. If the
motor turns the wrong way, swap pins 4 and 5 — no firmware change.

⚠️ **The driver module's middle pins read `Vcc` then `GND`** after
`In1`/`In2`, and the first draft of this layout had them the other way round.
Built like that, the DRV8833's supply is reversed: current flows straight
through its protection diodes, the laptop's port cuts power, and the Zero never
enumerates — "plugged in and nothing happens". That one survived; check the
module's silkscreen against H13/H14 before soldering.

**The DS3231 uses its 4-pin passthrough side**, `SCL · SDA · VCC · GND` on
J6 … J3. The first layout used the 6-pin side because the passthrough's order
is mirrored against it, and fitting the wrong side lands every pin on its
mirror — nothing damaged, but nothing answering. **One open I²C line and one
wrong order look identical** on the console, `rtc: nothing answered; running
without one`, so beep each module pin to the Zero's: SDA J5 ↔ S10 (GPIO18),
SCL J6 ↔ R10 (GPIO19).

**The diode-OR.** A dual common-cathode Schottky, **MBRF2045CT** (the part in
hand is marked `MBRF2045DT`, a vendor variant), drawn as `Q1` because DIYLC
has no dual diode: anodes on the two supplies (F3 USB, D3 battery), cathode —
the middle leg, E3 — on the rail. Whichever supply is higher feeds the board,
and neither can push current into the other, so USB can never charge the
alkaline cells. A plain wire from the battery pin to the rail would. Twenty
amps is far past anything here. The rail sits a Schottky drop below the higher
input — roughly 4.7 V on USB, falling with the cells on batteries — which the
Zero's regulator tolerates to about 3.5 V.

**On batteries, estimated rather than measured:** the C6 on Wi-Fi draws
80–120 mA, so three AAs are roughly a day — a power-cut bridge for the next
meal or two, not a way to run. The motor turns slower on cells, lengthening
the detent; the 2.5× jam budget should absorb it. The unit still cannot tell
which supply it is on — *no USB detection* stands.

**Grounds and 5 V are starred on C1** (E5 `+`, G5 `−`), not chained along a
rail. The header's ground reaches `−` before anything leaves it. Then:

- **the DRV8833 gets its own 5 V and its own ground run to C1**, because the
  motor's current flows through it. Shared with the Zero, the return current
  would shift the ground GPIO2 is read against, and the start-up dip would
  reach the Zero's supply;
- **the Zero gets its own pair**, and the DS3231 hangs off the Zero's ground
  and 3V3 as an ordinary rail. A shared trunk is harmless at tens of
  milliamps.

**Two guards on the switch line, one fitted.** The header is not keyed, the
switch pin sits beside motor 2, and the switch's run along row 17 passes the
5 V run on row 18. A plug reversed or a position off, or a blob between those
rows, would put 5 V on GPIO2. **R1, 1 kΩ in series** (J17–M17), limits that to
a current the pin's clamp survives, and changes nothing about reading a switch
that pulls to ground. It guards everything on the switch's side of it; the
GPIO2 side, M17 to R16, still runs beside row 18 and is what the beep test's
GPIO2 ↔ 5 V line is for. A 100 nF from GPIO2 to ground was considered and
**left out**: the 30 ms debounce and the spacing rule cope with the noise a
switch wire picks up in the motor's cable. Fit it only if spurious clicks ever
appear. Mark pin 1 on the housing and the board either way.

**Before first power, with the continuity beeper**, modules plugged in, holes
as in the drawing:

| Probes | Expect | Catches |
|---|---|---|
| E5 ↔ G5 (C1 + ↔ −) | silence | 5 V shorted to ground |
| W16 ↔ V16 (Zero 5V ↔ GND) | silence | the same, at the Zero |
| H13 ↔ H14 (DRV Vcc ↔ GND) | silence | the same at the driver — and a shorted driver |
| U16 ↔ V16 (Zero 3V3 ↔ GND) | silence | 3V3 shorted to ground |
| E5 ↔ U16 (5 V ↔ 3V3) | silence | 5 V on the 3V3 rail |
| F3 ↔ D3 (USB ↔ battery anodes) | silence | a bridge that charges the cells |
| A1 ↔ B1 (switch ↔ motor 2) | silence | a bridge at the header |
| R16 ↔ W16 (GPIO2 ↔ 5 V) | silence | the row-17 run touching row 18, past R1 |
| A1 ↔ R16 (switch ↔ GPIO2) | ~1 kΩ on the ohms range | R1 and the D17–F17 jumper in place |
| H11 ↔ U16 (`In1` ↔ 3V3) | silence | the 3V3 run beside `In1`: a bridge runs the motor on its own |
| T16 ↔ H11, S16 ↔ H12, U10 ↔ B11 (GPIO0 ↔ `In1`, GPIO1 ↔ `In2`, GPIO14 ↔ `Ult`) | beep | the runs and jumpers reaching the driver |
| J5 ↔ S10, J6 ↔ R10 (SDA ↔ GPIO18, SCL ↔ GPIO19) | beep | the RTC's lines, module pin to Zero pin |

Then USB alone: ~4.7 V across C1, 3.3 V on the Zero's `3V3`, and
`./dev/flash.sh --board zero --headless` showing `switch: watching GPIO2`.

The places a stray blob does damage are where two nets meet a hole apart:
5 V (F8–F14) beside ground (G7–G12) on the way to the driver, `Out1` (D9–D15)
beside 5 V (E9–E18), the 3V3 run beside `In1` at I10/I11, and rows 17/18.
Solder those with care.

**Finishing.** An insulating spray on the solder side, **after** the beep test
and a working power-up — it seals a bridge in rather than fixing it — kept out
of the header's contacts. In the case, components face outward, where the BOOT
pinhole, the USB-C and the RTC's coin cell are reachable, and the solder side
sits on printed bosses on the corner mounting holes, ~3 mm proud of a plate, so
clipped leads touch nothing. A drop of glue or a cable tie takes the cable's
pull off the header's joints.

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
from 1 ms to 10 s, rather than only for the mechanism on the bench.

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
- **Nothing gates on the jam flag, and that is what makes a jam recoverable.**
  `request`, `start` and `on_click` all behave normally while jammed, so the
  next feed request simply tries again, and the first click it produces clears
  the flag. The knob is one such producer — **hold two seconds to open the
  menu, then tap `Retry feed`** — so a jam never needs a power cycle, and never
  has.

  What that gesture lacked was any sign it had landed: `Status::of` reports
  `Jammed` over `Armed`, so the LED stays solid red through the whole hold.
  That ordering is deliberate and stays — red has to keep warning while
  somebody has their hands in the mechanism — so **the panel carries the
  confirmation instead**: `** JAMMED **` stays at the top while the menu is
  open, and the first item reads `Retry feed` rather than `Feed one portion`. A jam is one of the states `display::awake`
  never sleeps in — setup and a BOOT hold are the others — so the line is
  there whenever somebody walks over to look.

  The hint's duration comes from `button::ARM_HOLD_MS` through
  `display::hold_hint_for`, and is **rounded up** rather than to nearest: an
  instruction may overstate a hold but must never understate one, because
  holding longer than the printed time always arms and holding for exactly a
  floored figure need not. It is deliberately not spelled out here either, so
  this paragraph cannot become the copy that still says `2s`.

  ⚠️ **Retrying is not unjamming.** The motor has no reverse, deliberately
  (`motor.rs`: reversing "could jam the mechanism against its own geometry"),
  so a retry drives forward into the same obstruction and reports a jam again
  one budget later. It recovers a *transient* stall; a real blockage still
  wants a hand. Whether these mechanisms jam transiently at all is unknown —
  none has jammed yet — and that is a bench observation, not a design choice.
- Producers use `try_send` and log the discard, so a full queue never blocks
  the MQTT or clock task.
- Reading the switch level is I/O, so it is an input to `start` rather than
  something the machine works out. That is the only place the task supplies a
  fact rather than an event.

## Architecture — decided, do not re-litigate

- **The unit owns its clock and its schedule** — superseded 2026-09-25; this
  line used to read *no local RTC, no NTP, no flash persistence*. The clock is a
  DS3231 on a coin cell, corrected by every live `feeder/time` and settable on
  the knob; no NTP. The schedule is in flash, given by an explicit command
  (`feeder/<id>/schedule` or `feeder/all/schedule`) and never inherited
  from a retained topic, so a new unit starts blank. Offline → keep feeding on
  what it holds. Power-cycled with no broker → feed, if the RTC kept time and a
  schedule is stored; otherwise wait, never guess. See *A second version*.
- **No sleep modes, no USB detection** in v1. *No batteries* was part of this
  line until 2026-09-28: the feeders' own AA compartments are now a backup
  supply through a diode-OR, and never charged — see *The perfboard*. The
  firmware does not know which supply it is on.
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
  index would then point at a different meal. **Only a later date re-arms the
  day's slots**; a clock that goes *back* across midnight keeps the marker's
  time of day, so a date set wrong on the knob and then corrected cannot serve
  a meal twice. `a_date_corrected_backwards_does_not_serve_a_meal_again`.

## Provisioning

Credentials come from flash, and a unit with none raises its own Wi-Fi network
and serves a form. **Built**, and driven end to end from a phone: typed in,
saved, rebooted, joined the house network and reached the broker.

```text
  boot ── read the record from the nvs partition
           ├── usable  → station mode, connect, run normally
           └── missing, or no network in it → access point, serve the form, save, reboot

  knob's click held through power-on, or BOOT held 5 s while running
        → forget the network, keep the calibration, reboot   (lands in "no network")
```

**A reset forgets rather than erases** (since 2026-09-28). The record holds the
network *and* the unit's calibration, so erasing it lost the bench-measured
detent interval too — found the first time the BOOT reset was used, when the
unit came back on the 1900 ms default. `Record::without_network` blanks every
credential and keeps the two mechanical figures; the result fails
`is_usable`, so the boot path goes to setup mode exactly as for erased flash,
and setup mode's form carries the calibration into the record it saves.

**One way in, not two.** A reset rewrites the record rather than signalling,
so "no usable record" is the only state the boot path has to recognise. There is deliberately
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

Credentials are the one thing the broker cannot tell a unit, because they are
how it reaches the broker. The schedule now lives in the same partition, in a
sector of its own at nvs+0x1000 (magic `FDS1`) — see the architecture bullet
*The unit owns its clock and its schedule*. The timezone has the sector after
it, at nvs+0x2000 (`FDZ1`). Only `paused` is still broker state.

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

The reset gestures are already built — see *The outside button* below: the
knob's click held through power-on, and BOOT held 5 s while running. Both
forget the network, so the "no usable record" state is reachable without any
further work.

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
`store: no record yet, going to setup` instead and raises its own network.
A unit that was reset says `store: no network in the record (calibration
kept), going to setup`, which is the same path with the calibration carried.
There is no build-time fallback left, so anything else is a warning naming a
fault — `record is unusable`, `unreadable (…)`, or `no nvs partition`.

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
click to the next.

⚠️ **Measure it with a full hopper.** A loaded mechanism turns slower, so an
empty one gives the *fastest* the feeder ever runs, and the two constants do
not degrade symmetrically from there:

- **The jam timeout breaks.** It is `interval × 2.5`, so sizing it on the
  empty figure sizes it on the fastest case. Load the hopper, the detent takes
  longer, and a budget that looked like 2.5× becomes 2× or less. The failure is
  a **false jam** — motor stopped, portions discarded, meal dropped, solid red
  — arriving on refill day, which is precisely when it must not.
- **The minimum spacing is safe either way**, and provably so. It is
  `interval × 0.4`, so calibrating on the slow figure gives `I_full × 0.4`
  against a fastest real click of `I_empty`. That only misfires if
  `I_full / I_empty > 2.5`, which is a motor nearly stalled and a different
  problem entirely.

So calibrate on the slowest case and the fastest looks after itself; the
reverse is not true. Worth recording both figures while the hopper is open —
the ratio says whether 2.5× is the right multiplier for *this* mechanism, and
nobody goes back for that number later. Both other constants follow from it, and today's
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
that over every interval from 1 ms to 10 s: never within 4× the debounce, and
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
./dev/provision.sh --host <broker>      # ...and pointed somewhere else
```

### Per-unit portion size

**Built**, in the same record as the timing above.

`feeder/all/schedule` sends the same schedule to every unit, so a slot saying
`portions: 2` reaches every feeder as the same request. The feeders are
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

**It is the rotary encoder**: its shaft switch on GPIO3, `A`/`B` on GPIO4/GPIO5,
outside the case and distinct from the hub microswitch on GPIO2, which is
sealed inside the mechanism. This is fork (a) of *Version 1.5: the knob*,
taken. Two pure modules own it: `button.rs` turns the click into holds and
taps, and `menu.rs` decides what those and the knob's turns mean. `encoder.rs`
turns the two lines into detents.

| | Turn | Tap | Hold 2 s |
|---|---|---|---|
| **locked** | step the info pages | back to the home page | **unlock** → the menu, cursor on `Feed`. LED blinks cyan |
| **unlocked** | move the cursor | run the item | **lock** |
| **editing a number** | change it | save it; in force at once | lock, discarding the edit |
| **confirming a reset** | `Keep` / `Erase` | run the choice | lock, keeping everything |
| nothing for 10 s | | | locks again, discarding any edit |
| **held through power-on, 3 s** | | | forget the network, keep the calibration |

**The onboard BOOT button (GPIO9), held 5 s while running, forgets the network
settings** and reboots into setup mode — on every build, knob or not. The LED
flashes fast blue while it counts, and letting go before five seconds keeps
everything. Like the power-on gesture it forgets only the network — every
credential blanked, the rest of the record kept — so meals, calibration and
timezone stay. It needs no pin (BOOT is on every board)
and is safe to read at runtime because it only matters at reset, where it
selects download mode. It is the recovery for a unit that cannot reach its
network, when its admin page is unreachable for the same reason, and the
only one a headless unit has. `reset.rs` holds the rule, host-tested; the case
needs a pinhole over the button.

**A headless build**, `--features headless` (`./dev/flash.sh --headless`),
leaves out the menu, the encoder and the panel: GPIO3–GPIO5 are unused — not
even read at boot, so there is no power-on gesture and a stray bridge there
cannot forget the network — and the
unit is configured over its setup network, its admin page and Home
Assistant. Setup mode then shows its password only on the console, so the
sticker from `dev/ap-password.sh` is required rather than a backup. A build
flag rather than a panel probe, because a broken panel must not silently
turn the knob's click into something else. Verified booting headless on the
Zero. The BOOT hold was first used on hardware in its original form, which
erased the whole record and so lost the calibration — that is what changed it.
✅ **The current form is verified on `9a6ecc`, headless, 2026-09-30**: held
five seconds, the unit came back with `store: no network in the record
(calibration kept), going to setup` and `setup: keeping this unit's measured
timings from the old record`, and `provision.sh` put it back online.

The menu is `Feed one portion`, `Pause schedule` (or `Resume schedule`),
`Settings` and `Lock`. **Settings** holds this unit's calibration — `Portion`
(the portion scale, 25–300% in 5% steps) and `Detent` (200–10000 ms in 10 ms
steps) — then `Factory reset` and `Back`.

- **Saving does not restart.** `Store::update` rewrites the record with
  everything else untouched, then `Bus::calibration` carries the new figure to
  the feeder task, which applies it with `Feeder::recalibrate` at its next idle
  moment — **never mid-turn**, because one turn's jam budget and pending clicks
  were promised under the old figures. The console repeats the
  `feeder: clicks >…` line when it lands. A restart would have cost the live
  time, the `fed` line and ~12 s off the broker, and cut short a feed in
  progress. Wi-Fi and broker edits, when they exist, will still need one: the
  network stack is built once at boot.
- **A tap on an unchanged value just goes back**, with no flash write.
- **Factory reset erases more than the boot gesture does**, behind a
  `Keep`/`Erase` choice that starts on `Keep`. `Store::erase_all` takes the
  credentials, the calibration *and the meals* — the panel says `erases Wi-Fi,
  broker,` / `calibration and meals` — whereas the two reset gestures forget
  only the network and keep the calibration, the schedule and the timezone. Both land
  in setup mode by the same path.
- **`Clock` sets the date and time by hand** — year, month, day, hour,
  minute, a tap between each, the tap on the minute sets it with seconds at
  zero. It goes in as `TimeSource::Manual`, which arms and overrides like a
  live time, and to the RTC; Home Assistant's next live time still has the last
  word. The home page shows `now HH:MM` while the clock is trusted, so the
  result can be checked at the feeder. Settings is five items against four
  rows, so its list scrolls.
- **Wi-Fi, broker and schedule are not editable here yet.** Wi-Fi and broker
  wait on a decision about text entry. Who owns the schedule is decided and
  built — the unit, in flash — but it still arrives only over MQTT; a knob
  editor for it is not written. Clockwise moves down the menu and forward through the pages, and both
lists stop at their ends rather than wrapping, so turning left until it stops
always lands on `Feed`.

**Hold toggles the mode; a tap does whatever the mode means.** That is the whole
vocabulary. The rules that keep it honest, each a host test in `menu.rs` or
`button.rs`:

- **Turning never dispenses.** The only way to food is a hold, then a tap on
  `Feed`. A cat batting the knob steps pages and lights the screen.
- **`Feed` stays under the cursor after feeding**, so three portions is three
  taps. No count is held between taps — see *Manual feeds accumulate*.
- **Unlocking always starts on `Feed`**, never on the item left last time, so a
  hold then a tap does the same thing every time.
- **A lock has to come from a different press than the unlock.** Unlocking fires
  *while* the button is still held, so without a per-press latch a four-second
  hold would unlock at two seconds and lock at four, and read as a button that
  does nothing. `one_long_hold_arms_once_and_does_not_also_lock` pins it.
- **Turning keeps the menu open**, because somebody reading it is not done.
  Turning against an end stop still counts as attention but redraws nothing.
- **Waking always shows the home page.** A turn on a dark panel only lights it;
  otherwise the first thing seen is whatever page was left days ago.

**Pause from the menu is published, not just applied.** The flag lives in the
retained `feeder/<id>/paused` topic, so setting it locally alone would be undone
by the replay on the next reconnect, and Home Assistant's switch would disagree
until then. `Bus::pause_request` carries the wish to `mqtt.rs`, which publishes
it retained — or, if the broker is unreachable, publishes it on reconnect
*before* subscribing, so the replay carries the new value back.

**Home Assistant stays the authority — decided 2026-09-25.**
`cat_feeder_pause_when_away` in the package republishes every unit's `paused`
from `schedule.cat_feeder_active` on every Home Assistant start and every change
of that helper, and knows nothing of the knob. So the knob's pause is a
**temporary override**: it lasts until the next Home Assistant restart or
helper change, then reverts to what the helper says. Worth knowing when a
feeder resumed by hand turns up paused again — that is Home Assistant, not a
fault.

**The adversary is cats, not clumsiness.** A button on the outside of a cat
feeder that dispenses food when pressed is a button cats will learn to press —
food is the strongest reinforcer there is and a cat has all day to experiment.
That is the whole reason for arming, and it is why a tap alone does nothing.
A knob cannot be recessed the way a button can, so **placement** is the
mechanical defence now: the case can sit high or behind the feeder, where a paw
has no footing.

**Reset is a boot gesture on purpose.** Sharing one button between feeding and
erasing means separating them by hold duration, and the failure mode writes
itself: hold a beat too long on a working feeder and its credentials are gone,
with three units already screwed into place. Requiring a power cycle means it
cannot happen by accident at all. It costs one GPIO read on an ordinary boot —
only a boot that begins with the button held waits the three seconds.

Arming outranks the **network** faults on the LED. The case that settles it: the
broker is down, which is precisely when manual feeding matters, and being
re-told the network is out is less useful than seeing that the menu is open.
Whatever it hides is still there ten seconds later.

**A jam is the exception, and it beats arming.** Red keeps warning because
somebody may have their hands in a hub that a tap can start turning, which no
ten-second window makes safe. The cost is that arming a jammed feeder shows
nothing at all on the LED — so the panel keeps `** JAMMED **` over the menu and
offers `Retry feed` instead, and
*The feeder task owns the motor* has the rest.

(This used to read "every fault", which was wrong for the one fault where
manual feeding matters most. `indicator.rs`'s test is now named
`an_armed_button_outranks_the_network_faults` to stop it drifting back.)

**Verifying this needs a phone.** The console can show the access point
starting, a station associating, and a request arriving, but joining the network
and submitting the form is not something `dev/flash.sh` can do. Expect a round
or two of iteration on the parts only a real client exercises — captive-portal
probes, keep-alive, and browsers that open several connections at once.

## MQTT contract

Broker: Mosquitto (HA add-on / Docker), port 1883, user/pass. The dev broker
runs in Docker from this repo's `compose.yaml`; a deployed one is wherever that
install lives, and nothing here needs to know.

```
feeder/<id>/availability   online | offline        (retained, LWT = offline)
feeder/<id>/feed           <portions:u8>           cmd, manual feed
feeder/all/feed            <portions:u8>           cmd, all units at once
feeder/<id>/paused         ON | OFF                retained, pause the schedule; from HA or from the unit's menu
feeder/<id>/schedule       [{"time":"08:00","portions":2}, ...]   cmd, NOT retained, this unit's meals
feeder/all/schedule        [{"time":"08:00","portions":2}, ...]   cmd, NOT retained, every unit's meals
feeder/<id>/schedule/state [{"time":"08:00","portions":2}, ...]   retained, what the unit holds
feeder/<id>/meal/<n>/time      08:00:00            cmd, NOT retained, meal n's time (n = 1..8)
feeder/<id>/meal/<n>/portions  2                   cmd, NOT retained, meal n's portions; 0 switches it off
feeder/time                2026-09-14T08:00:00+02:00             retained, from HA, every minute
feeder/time/request        <id>                    cmd to HA, NOT retained
feeder/<id>/state          {"feeding":bool,"jammed":bool,"paused":bool,"meals":n,"last_fed":"..."}
feeder/<id>/event          {"event_type":"scheduled","portions":2,"slot":"08:00","at":"..."}   NOT retained
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

The **offset is kept, never converted away**. Home Assistant publishes its own
local time and the feeders live in the same house, so the wall-clock fields
already arrive in the frame the schedule is written in: `08:00` in a slot means
08:00 on the kitchen wall. While Home Assistant publishes, daylight saving
costs nothing — in October it simply starts sending `+01:00` and the wall-clock
fields shift with it — and its offset outranks the unit's own timezone, below.

**A unit can keep summer time on its own**, for when nobody publishes the time
— built 2026-09-28, see *Timezone* under *A second version*. That is the one
place an offset is *applied*: moving the clock an hour at the change, and
only after ten minutes with no live time.

The assumption this rests on is **the broker and the feeders share a
timezone**. The one realistic way to break it is publishing `utcnow()` instead
of `now()`, which would still look like a valid time while moving every meal by
the offset. That is why the offset is kept and printed at startup
(`clock: live time 2026-09-18T00:07:18+02:00, schedule armed`) rather than
dropped: it turns a silent hour-long error into the first line on the console.

**What the unit did reaches Home Assistant's Activity** through the `event`
entity, *Feeding*, on `feeder/<id>/event`: `scheduled` when a slot is queued,
`skipped` when one passes paused or too late, `manual` for a feed at the knob
or the admin page, `jammed`. Feeds sent *from* Home Assistant raise nothing,
because Activity already has the press. Each carries the unit's trusted time
as `at`, since one raised with the broker down is queued and arrives late.
`events.rs` has the table.

`last_fed` is reported the same way, local with the published offset, and
covers scheduled feeds only — a manual feed reaches the feeder task, which has
no clock, and Home Assistant already records button presses in its own history.

Home Assistant MQTT discovery: on connect, publish **retained** config to
`homeassistant/<component>/feeder_<id>/<object>/config` for: a `button`
(feed), a `switch` (paused), a `binary_sensor` (jammed), an `event` (feeding),
and eight `time` plus
eight `number` entities, *Meal n time* and *Meal n portions* — the schedule
editor, v2's point 4. All share the same `device` block so HA groups them into
one device. Then publish `online`. The payloads are rendered by `discovery.rs`,
which is pure so a host test can parse every one as JSON and measure it against
the buffer; its module doc has the entity table.

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
- Retained because `paused` is not in flash — unlike the schedule, it is still
  broker state: a unit that reboots while paused must come back paused.

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
| BOOT held towards a reset | blue, one flash every 0.4 s — let go to cancel |
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
- `./dev/flash.sh [--seconds n] [--filter re] [--board devkit|zero] [--headless] [--port p]`
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

Mosquitto + Home Assistant in Docker on this machine, so firmware work needs
nothing else. Details and troubleshooting in `dev/README.md`.

```sh
docker compose up -d        # broker on 1883, HA on http://localhost:8123
./dev/watch.sh              # tail feeder/# and homeassistant/#
docker compose down -v      # stop and wipe every retained message
```

Same broker, three addresses: `localhost` from this machine, `mosquitto` from
the Home Assistant container, this machine's LAN address from the ESP32 — which is why
`cfg.toml` says `mqtt_host = "auto"` and `dev/provision.sh` resolves it when it
builds the record. That address is a DHCP lease and moves; a unit provisioned
before a move sits flashing red twice, which is correct for "no broker" and
looks exactly like a broker that is down. `down -v` wipes every retained
message, but it is **no longer a cold boot**: the schedule is in the unit's
flash and the time is in its RTC, and both survive it. A true cold start is the
menu's Factory reset, which erases the meals with the credentials, plus pulling
the DS3231's coin cell so its oscillator-stop flag comes back set.

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
  reset.rs        pure logic: the BOOT button's 5 s hold to forget the network
  button.rs       pure logic: holds and taps of the knob's click, arming
  menu.rs         pure logic: pages while locked, the menu while unlocked
  encoder.rs      pure logic: the knob's A/B levels into detents
  indicator.rs    pure logic: what the LED shows, the priority ladder, the
                  blink timing
  led.rs          the WS2812 itself, over RMT. Colours in, bits out
  discovery.rs    pure logic: the Home Assistant entities and their retained
                  discovery configs, JSON-checked on the host
  events.rs       pure logic: what goes on feeder/<id>/event for Activity —
                  meals served or skipped, feeds at the unit, jams
  tz.rs           pure logic: a timezone's POSIX rule, the offset at an
                  instant, and the stored zone
  display.rs      pure logic: the six lines the screen shows — home, info
                  pages, menu, setup — and when the panel is lit
  oled.rs         the SSD1306 itself, over async I2C. Text in, pixels out
  mqtt.rs         connection, LWT, discovery, subscriptions, state publishing
  ds3231.rs       pure logic: the DS3231's registers — time, OSF, EOSC,
                  temperature
  rtc.rs          the DS3231 over the shared I2C bus
  i2c.rs          the one I2C bus, shared by the panel and the RTC
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
                  form served over http.rs
  http.rs         the sockets under both web servers: reading a request,
                  answering it, and the buffers they share
  admin.rs        pure logic: the admin page — who may (Basic auth with the
                  derived password, same-origin POSTs), the schedule and
                  network forms, and the page itself
  web.rs          the admin page on the house network, over http.rs
  config.rs       Config, from a flash record
build.rs          injects ap_secret from cfg.toml, and nothing else
examples/mkrecord.rs
                  host-only: builds a provisioning record for dev/provision.sh

pcb.diy
                  the perfboard, in DIY Layout Creator, seen from the
                  component side; see *The perfboard*
homeassistant/packages/cat_feeder.yaml
                  the other half of the system: publishes the time, the
                  send-the-schedule script, the pause helper, the feed-all
                  script. Tracked here and used
                  unchanged on any instance; install per dev/README.md
```

Embassy tasks: `net` (Wi-Fi + stack), `mqtt`, `switch` (owns the GPIO),
`feeder` (owns the motor), `schedule` (owns the clock, ticks once a second and
re-aligns), `rtc` (owns the DS3231: logs it at boot, arms the clock from it,
keeps it set from live times), `encoder` (reads the knob's `A`/`B`), `ui`
(owns the knob's click, the menu and the store's writes from it), `display`
(owns the panel), `indicator` (owns the LED), `web` (the admin page), `reset`
(owns BOOT on GPIO9, and forgets the network after a 5 s hold). `encoder`,
`ui` and `display` are left out of the headless build. They
communicate through the one
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
   ✅ **Verified on the assembled mechanism**, and it behaved: the hub parks
   with the switch closed (`switch: watching GPIO2, currently pressed`), so no
   run needed aligning; three taps gave three clicks with `pending=` rising
   mid-turn and the motor never stopping. That is the align/accumulate design
   confirmed outside the bench button for the first time.

   ⬜ **Still to do: re-measure the detent interval with a FULL hopper.** An
   empty one gave 2038–2061 ms across five intervals, mean ≈ 2048 ms — but a
   loaded mechanism turns slower, and that is the figure the constants must be
   derived from. See *Per-unit mechanical timing* for why the direction of the
   error matters. Until then this unit runs on the 1900 ms default, which is
   ~8% fast but breaks nothing.
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
   in `schedule.rs`, host-tested, and every rule verified on hardware by
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
7. ✅ Home Assistant: an automation publishing time (every minute), a script
   sending the schedule to `feeder/all/schedule` when run by hand, the pause
   helper and a feed-all script, in
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
   drilled. The encoder is already wired — see *Version 1.5: the knob*

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
- **A set RTC is a rung of its own, and it arms** — decided 2026-09-25. At boot,
  a DS3231 whose oscillator-stop flag is clear hands its time over as
  `TimeSource::Rtc`, which earns trust like a live time: the schedule arms about
  a second after power-on without waiting for Home Assistant. It never
  *overrides* a trusted clock — once a live time has arrived, an RTC read is as
  stale as a retained one — and the RTC is rewritten from live times, not the
  other way round. An RTC with the flag set counts for nothing, which keeps
  *power-cycled and no broker → wait, never guess* intact at the bottom rung.
  Verified with Home Assistant stopped: `clock: RTC time …, schedule armed` at
  1.4 s. With the schedule now in flash as well, it **feeds with no broker at
  all**: a set RTC and a stored schedule are everything a scheduled meal needs.
- **The baseline waits for a schedule.** An RTC could trust the clock before
  any schedule was loaded — it comes from flash at boot, or from a command for
  a blank unit — and a first look spent on no slots left
  the real schedule to the lateness limit — a reboot at 19:01 would have served
  the 19:00 meal again. `Scheduler` now takes its baseline only once a schedule,
  even an empty one, has arrived. `the_baseline_waits_for_a_schedule_to_arrive`
  pins it; seen on hardware as `slot 19:00 already past at startup`.

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
will **not feed at all** until Home Assistant returns *if its RTC cannot be
trusted* — oscillator-stop flag set, coin cell flat or never fitted — or if it
has no schedule stored. A unit with a set RTC and a stored schedule arms at
boot and feeds regardless.
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

**It degrades correctly**, which matters for any Home Assistant that does not
have the package installed yet: a unit whose request nobody answers simply
waits for the next `/1` publish, which is the old behaviour. The two halves are
therefore independent, and a feeder repointed at an instance before the package
reaches it loses the speed-up and nothing else.

**`mode: single` on that automation is fine.** Three feeders rebooting together
send three requests within milliseconds and Home Assistant will drop two of
them — but the one publish that does happen is forwarded live to all three
subscribers, so every unit is served.

### Mesh networks: join the strongest node

The house network is a mesh — one SSID, several nodes, `192.168.68.x` — and
esp-radio's `StationConfig` defaults to `ScanMethod::Fast`, which in ESP-IDF
joins the **first** node answering to the SSID rather than the best. A Zero a
metre from one node was seen associated at −82 dBm, disassociated for
inactivity, and hanging in its MQTT TCP connect; the laptop could not ping it.

`main.rs` sets `ScanMethod::AllChannels`, under which the
`WIFI_CONNECT_AP_BY_SIGNAL` sort esp-radio already sets actually applies. The
cost is a full scan at each connect, a second or so. It chooses at connect time
only — the station does not roam — which for a feeder that never moves is the
right trade.

✅ **The MQTT connect is bounded.** The TCP connect and the CONNECT/CONNACK
exchange each get `CONNECT_TIMEOUT` (10 s) in `mqtt.rs`, and a timeout lands in
the ordinary 5 s retry instead of hanging — which is what had turned a bad link
into a unit silent for over 100 s. Verified by pausing the dev broker's
container, which accepts TCP and never answers CONNECT: `mqtt: no CONNACK
within 10s` twice, then a normal connect 0.8 s after unpausing.

### The other ten seconds: a lost DHCP DISCOVER

✅ **Fixed: DISCOVER is resent after 2 s instead of 10.** `dhcp_config()` in
`main.rs` sets smoltcp's `RetryConfig::discover_timeout`, which embassy-net's
`DhcpConfig` exposes. `associated` → `connected` went from ~10 000 ms to
**2029 ms** on the first capture — one retry, so the first DISCOVER is still
lost, which confirms the cause below rather than disproving it. The ordering
fix it suggests would recover the last two seconds; not worth it yet.

The analysis that led there:

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

     Look for `store: network forgotten by the boot button, calibration kept`, then `setup: raising
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

11. Deploy against an always-on Home Assistant. The feeders talk to the
    development stack in `compose.yaml`, which runs on a laptop that is not
    always on; cats need one that is.

    **This repo does not know which instance that is**, deliberately. There
    will be more than one over this project's life — a spare box, a rebuild, a
    different house — and an address written down here is an address that goes
    stale in seven files at once. What the repo knows is the dev stack it
    ships, and that a unit is pointed elsewhere with flags. Addresses,
    hostnames, filesystem layouts and credentials live with the deployment.

    What that instance has to provide, in the order it has to be true:

    - **Mosquitto** with `listener 1883`, `allow_anonymous false`, a user for
      the feeders, and — the one easiest to omit — `persistence true`. Without
      persistence every retained message is lost on a broker restart. The time
      comes straight back, because the package republishes it each minute, and
      the schedule is in each unit's flash rather than the broker's; but
      `feeder/<id>/paused` is still broker state and does not come back, and a
      paused feeder silently resuming is the one state change nothing alarms
      about.
    - **Home Assistant onboarded, with the right timezone.** Onboarding is what
      sets it, and an instance left on UTC publishes a payload that is entirely
      valid with every meal moved by the offset — the silent hour-shift under
      *MQTT contract*. The offset on the wire is what proves it, not a setting
      you can read back.
    - **The MQTT integration configured.** An instance run with
      `network_mode: host` reaches its broker at `localhost`, not at a
      container name — a common deployment style, because Bluetooth and mDNS
      want it. The package publishes through this integration, so without it
      every automation in it fails at runtime while the package itself loads
      cleanly.
    - **`homeassistant/packages/cat_feeder.yaml` installed**, unchanged — it is
      tracked here precisely so it can be — **and a `homeassistant: packages:`
      include added**, which an instance set up through the UI does not have and
      without which the directory is never read.

    ⚠️ **The package goes in before any feeder is repointed.** It is the half of
    the system that publishes `feeder/time` every minute. A feeder pointed at a
    broker where nobody publishes it takes the *retained* time, starts its clock
    on it, and never arms the schedule — see *A retained `time` is not a trusted
    one*. A unit whose RTC was never set — or whose coin cell went flat — sits
    `online`, flashing red ×3, and does not feed. That is exactly correct
    behaviour and indistinguishable from a bug. (A unit with a set RTC arms from
    it, but still wants live times to stay corrected.)

    It is checkable with no feeder involved at all, which is why the ordering
    costs nothing:

    ```sh
    ./dev/watch.sh --host <broker> --user <name> --password-file <path> \
      'feeder/time'
    ```

    A line a minute means that half is done; silence means Home Assistant is up
    but the package is not loaded. Read the offset before believing it. It
    doubles as a credential test, so a wrong password fails here rather than
    silently inside a unit.

    Then **repoint the feeders, which is a re-provision and not a rebuild**:
    `mqtt_host`, `mqtt_user` and `mqtt_password` all live in each unit's flash
    record, so it is one `./dev/provision.sh --host <broker> --user <name>
    --password-file <path>` per unit and no compile. `provision.sh` erases only
    the credentials sector, so a unit keeps any meals it already holds.

    **Then give each unit its meals — the package alone no longer does it.** A
    unit owns its schedule, and a new or factory-reset one starts blank
    (`NO MEALS SET` on the panel, `"meals":0` in its state). Once
    `feeder/<id>/availability` reads `online`, run
    `script.cat_feeder_send_schedule` from Home Assistant, which publishes the
    package's `meals` to `feeder/all/schedule`, not retained. Check that
    `feeder/<id>/schedule/state` echoes it and that the state payload's `"meals"` is
    above zero; until both are true the unit is healthy and will never feed.

    Give that broker's machine a **DHCP reservation** first. There is no
    resolver in the firmware — `mqtt.rs` parses `mqtt_host` with
    `Ipv4Addr::from_str` — so an address that moves takes every feeder off the
    air with no way back but re-provisioning each one.

    ⚠️ **A unit points at one broker.** Keeping the dev stack is fine; moving a
    feeder back and forth is not. The schedule travels with the unit now, in
    flash, but `paused` is still a retained message, so a unit returned to the
    dev broker picks up whatever *that* one last held — quite possibly a pause
    from last week, which is indistinguishable from a current one.

### What is left

Everything not yet built or not yet seen, in one place, as of 2026-09-28.
Anything absent from this list is done and verified on the Zero; each row
points at the section with the detail.

**Software**

| Item | State | Where |
|---|---|---|
| Schedule editor on the knob | not built — the schedule arrives only as an MQTT command | *Version 1.5: the knob* |
| The admin page in a phone browser | used from a desktop browser (calibration run, page layout); still unseen: *Feed now* turning the motor, *Use this device's time*, the timezone list, and a phone layout | *A second version*, point 5 |
| Changing a `Meal n` entity from Home Assistant's own UI | built; driven with the payloads HA's platforms send, not from its UI | point 4 |
| Wi-Fi and broker entry on the knob (character picker) | parked, deliberately | *Version 1.5: the knob* |
| Subsets of feeders by HA label | Home Assistant side only; no firmware change | point 6 |
| Optional "copy this feeder's schedule to those" blueprint | not written; only sensible after point 4 | *What Home Assistant is left doing* |

**Seen only in host tests, not yet on the hardware**

| Item | How to see it |
|---|---|
| The setup-mode screen (SSID, password, address) | hold the button through power-on, `./dev/capture.sh --seconds 40` |
| The `WI-FI`, `BROKER` and `DEVICE` info pages | turn the knob while locked |
| The BOOT hold's `HOLD TO ERASE WI-FI` banner, on a unit with a panel. The reset itself is verified, headless, on `9a6ecc` | hold BOOT on a knob unit; it forgets Wi-Fi, so re-provision after |

**Hardware and deployment**

| Item | Waiting for |
|---|---|
| Detent interval measured with a **full** hopper | a full hopper; then set it on the knob's `Detent` |
| The third feeder, on `99177c` and the `pcb.diy` layout: detent measured at 5330 ms on USB with an empty hopper. Left: the **full**-hopper figure, and the portion ratio against the other two | a full hopper; a scale or a measuring spoon |
| Unit two, `9a6ecc`, in its feeder and on the Pi: its meals sent; the detent measured by *Run calibration*, on batteries and on USB | a schedule; a calibration run each way |
| Unit three: soldered, flashed, provisioned, sent its meals | soldering |
| Motor direction checked on a real mechanism before bolting anything | each unit, before step 8 |
| The printed enclosure, then retiring the old PCBs (step 8) | CAD |
| Deploying to the Pi (step 11): the package is installed and publishing, and `9a6ecc` and `99177c` are repointed. Left: **meals sent** to every unit from the Pi | a schedule from the Pi |
| LED palette tuned in a real kitchen; an external WS2812 on GPIO8 | a unit in its place |

### What is waiting on what

| Step | Waiting for |
|---|---|
| 3, the detent interval | **nothing — the bridge is wired and driving**; it needs the motor on a real mechanism |
| 6, flashing the three Zeros | **nothing — the boards have arrived**, jumpers to be soldered |
| 8, retiring the PCBs | 3, the third feeder's interval measured, **and an enclosure designed and printed** |
| 11, deploying | an always-on Home Assistant with the package installed, then the schedule sent to each unit; the checklist in step 11 is the whole of it |
| a display | **nothing** — a 0.96" 128×64 SSD1315 is on the bench, working, and is the production part |
| the enclosure | v1.5 being settled, since the panel and any knob are most of what it holds |

**Every part is now on the bench**: the three Zeros, the DRV8833 and a display
to develop against. Nothing in this project is waiting on the post any more.

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

The lost DHCP DISCOVER is fixed down to a 2 s retry, and `feeder/time/request`
took the other forty-nine seconds out. Added up from the parts measured —
~2.9 s scan and association, ~2 s DHCP, ~1 s to the broker and a live time — a
boot should reach an armed schedule in about 6 s; not yet captured end to end.

Later (not now): a short press on the GPIO3 button feeding one portion, so a
manual feed works with the broker down. (Battery backup, once on this list,
is now hardware only — see *The perfboard*.)

**A display. The part is ordered.** The original LCD window is 40 × 18 mm,
which pointed at a 0.91" 128×32 I²C OLED — roughly a 38 × 12 mm module, two
pins, a 512-byte framebuffer, and **three** lines of 21 characters. Five of
them are on the way (SSD1306, I²C, `GND · VCC · SCL · SDA`). The common 0.96"
128×64 was the wrong shape for that window: its module is near enough square at
27 mm tall and would not go in.

⚠️ **That window is no longer the constraint** — see *The electronics live in
their own case*. A printed enclosure has no window to match, and the sizing
argument above is kept as the reasoning that was true while the original shell
was, not as a live requirement.

✅ **Settled: a 0.96" 128×64 SSD1315**, register-compatible with the SSD1306
driver and working on the bench. The 1.3" part never drew (see below), and the
0.91" 128×32 fallback was dropped with it: `display::ROWS` is **6**, the
`panel-128x64` feature is gone, and `oled.rs` initialises 128×64
unconditionally.

**Twenty-one columns is still the hard limit.** Both panels are 128 pixels
wide, so `FONT_6X10` gives 21 characters and `display::COLS` is 21; the new
panel bought rows only. `Line` is `String<COLS>` and `push` truncates silently,
so an over-long line is lost on the glass with nothing said on the console.

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
JOIN THIS WI-FI
cat-feeder-99177c
H75T-C7VT-6FAV

THEN BROWSE TO
http://192.168.4.1
```

`main.rs` spawns `display_task` **before** calling `setup::run`, because that
function never returns and could not spawn anything afterwards. The panel is
brought up before the boot decision for the same reason: both outcomes want a
screen and only one of them can come back for it.

Three facts, and the last is the address — which is now one constant. It used to be written twice, as an `Ipv4Addr` in `setup.rs` and as a
string in `display.rs` with a comment asking them to agree. `provisioning.rs`
holds `AP_ADDR_OCTETS` beside the SSID and password derivations, the socket is
built from it, and a host test pins the printed URL against it. A panel
confidently showing an address nothing answers on would be worse than no panel.

The salt is still needed — it is what stops a stranger deriving the password
from the MAC in the beacon — and the sticker drops from required to backup **the
day a capture shows the setup screen on a panel**, not before. Until then it is
the only thing that works, which is why `README.md` still tells a reader to
print one. A unit whose screen turns out blank on the production part, with no
sticker and no serial cable, cannot be joined at all.

It does not make the LED redundant: you read a screen standing at the
feeder, while the LED answers *is anything wrong* from the doorway. Burn-in over years of showing `next 08:00` is
handled by sleeping and waking on a press, and the collision that used to imply
— a press that both wakes the screen and feeds — is resolved by the gesture
table above: only a tap on `Feed`, in a menu opened by a hold, feeds. Night glare is the same blanking. The
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

A configured feeder timezone (`Europe/Rome`) — **built**, 2026-09-28, and not
the way this note first imagined: no tz database on the device. See
*Timezone* under *A second version*.

### Version 1.5: the knob

✅ **Built as fork (a)** — see below and *The outside button*. The RTC half is
not. It sits between the working prototype and
the printed case: after the mechanism is proven and before the enclosure is
drawn, because a panel and a knob are most of what an enclosure is *for* and
designing one around a bare board twice is the wasteful order.

The encoder is on GPIO3 (click) and GPIO4/GPIO5 (`A`/`B`); the GP20–GP22
reservation it used to hold is released.

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
whole argument. It is *not* nicer page-stepping, although turning is now how the pages in *The
screen's pages* are stepped.

**It removes SNTP rather than adding to it**, which is the opposite of what a
second input device usually does. A knob-set clock is set in local wall-clock
time, the schedule slots are already local wall-clock time, so nothing converts
and no timezone rules go on the device. DST is someone turning a knob twice a
year, exactly like every oven in the house. `feeder/time` stays as a convenience
for units that have a network. (Since 2026-09-28 a unit given a timezone on its
admin page does even that itself — see *Timezone* under *A second version*.)

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

✅ **Bring-up done and verified on the Zero, 2026-09-25.** `ds3231.rs` decodes
the registers (pure, host-tested), `rtc.rs` moves them, and `i2c.rs` shares
GPIO18/19 between the panel and the RTC through `embassy-embedded-hal`'s
`I2cDevice` — which moved this crate to `embassy-sync` 0.8, the version that
crate is built on. `rtc_task` logs what the chip holds at boot, and writes it
from a **live** `feeder/time` only when `OSF` is set or it is more than 2 s out.

Seen on hardware: a factory-fresh module read `2000-01-01`, `OSF` set, 26 °C;
it was set on the first live time; then, after a minute with **everything**
unplugged, it booted reading the right time to the second with `OSF` clear,
and was not rewritten. The cell is a **LIR2032**, which the module's charging
circuit is meant for — so no resistor to lift, but keep the module on `3V3`,
because that circuit charges from `VCC` and 5 V overcharges a LIR2032.

✅ **The trust ladder is settled**: a set RTC arms the schedule. See *A retained
`time` is not a trusted one*, which carries the rule and the double-feed guard
it needed.

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

Both fractions were counted from the seven spent before the encoder — GP0–GP3,
GP14, GP18, GP19 — and against thirteen usable pins rather than today's fifteen.

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

✅ **Taken: fork (a), 2026-09-25, and built.** The shaft switch is on GPIO3 and
`A`/`B` on GPIO4/GPIO5; the menu is in `menu.rs`, host-tested. **Seen on the
Zero:** holds unlocking and locking, the cursor following the knob with one
step per detent and clockwise moving down, `Feed` dispensing a portion per tap,
`Pause` toggling its label, and `Lock` locking. The info pages were not yet
turned through on hardware. The costs above now apply rather than being
hypothetical — the knob cannot be recessed, and a cat reaching the knob can
reach the menu. Placement is what answers them. GP20–GP22 are free again.

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

**Under fork (a), a hold could have come to mean two things by context** —
open-or-close on the home screen, back-or-save in a settings editor. As built it
does not: a hold only ever unlocks or locks. The settings editors still to come
must keep it that way, and confirm with a tap on an explicit item rather than
with a hold.

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

**Built, all but the Home Assistant half of point 6** — see the status
paragraph below. It overturned what used to read *No local RTC, no NTP, no
flash persistence*, and meant to: that rule is right for a system whose only
user owns the broker, and wrong for a feeder somebody else is given. What follows is one decision with
seven consequences, not seven options.

The question that forces it: what does a person who did not build this have to
install before the feeder works? Today the answer is a YAML package in Home
Assistant, and without it the unit connects, publishes discovery, shows a Feed
button that works — and never feeds, sitting `online` flashing red ×3. That is
correct behaviour and indistinguishable from a fault. Every comparable product —
PetLibro, SureFeed, Aqara, Shelly, Tasmota, an ESPHome device — answers it the
same way: **the device owns its configuration and the app is a control surface**.

✅ **Points 1, 2, 4, 5, 6 and 7 are built** (4 and 5 on 2026-09-28, the rest
on 2026-09-25), and verified on the Zero
against the dev broker: a schedule command is stored in its own flash sector
(`FDS1`, `store.rs`) and put in force; an identical one is not rewritten; a
reboot brings it back from flash; a *retained* command replayed at subscribe
time is refused, which is what stops a new unit inheriting meals; the unit
echoes what it holds on `feeder/<id>/schedule/state`, and `"meals"` in the state
payload and `NO MEALS SET` on the panel make blank visible. The package's
schedule automation became a script, *send the schedule to every feeder*,
publishing `feeder/all/schedule` — no longer on every Home Assistant start.
Point 3 is done by the RTC and the knob. Point 4 is the sixteen `Meal n`
entities and point 5 the admin page — both below. Subsets by label, in point 6,
are still Home Assistant's business and unbuilt.

**Point 5 as built.** `http://<unit>/`, on every configured unit, alongside
MQTT: a status block (clock, meals a day, paused, next meal, last fed, a jam),
then **Feed**, the eight **Meals**, the **Clock**, the **Calibration** and the
**Network**, each its own form. Everything but the network is what the knob
can already do, by the knob's own paths:

- **Feed** puts 1–`MAX_CLICKS` portions on the feed queue, like a tap on
  `Feed`.
- **Clock** takes a `datetime-local`, or the browser's own time with one
  button, and hands it to `Bus::set_clock_by_hand` — the one function the
  knob's `Clock` now calls too. `Manual`, so Home Assistant still has the
  last word.
- **Calibration** stores the detent and the portion scale with
  `Store::update`, then sets `Bus::calibration`, held to the knob's ranges and
  steps. The feeder applies it at its next idle moment. **The menu now reads
  `Bus::calibration` before every input** (`Menu::set_calibration`) — before
  that it kept its own copy, and a knob save after a web save would have
  written the web's figure back.
- **Run calibration** signals `Bus::calibrate`, after a confirm dialog, since
  it dispenses five portions. The feeder publishes the run on
  `Bus::calibration_progress` (a `Progress` from `calibrate.rs`), because the
  two signals the menu follows have one reader each; the page redraws itself
  every two seconds while it turns, then offers *Save N ms* when the result
  differs from the figure in force. A run started here does not disturb the
  knob, whose menu ignores results it did not ask for.
 `admin.rs` decides
everything and is pure; `web.rs` serves it over `http.rs`, the socket code
setup mode already had, moved out so both use it.

- **Basic auth, any username, the derived password** — the setup network's,
  the sticker's, `dev/ap-password.sh`'s. Wrong or missing is `401` with a
  challenge, so a browser asks.
- **A POST whose `Origin` is not this unit is `403`.** A logged-in browser
  resends Basic credentials on a form another page submits here; no `Origin`
  at all is `curl`, which has to bring the password itself. `parse_head`
  refuses an `Origin` too long to hold rather than dropping it, since empty is
  what passes.
- **Stored passwords are never on the page**: `admin::Network` has no field for
  one, both boxes render empty, and **empty means keep** — so an open Wi-Fi
  network is set through setup mode, not here. A rejected form comes back
  with what was typed, less the passwords.
- **Meals go through `Bus::schedule`**, the same queue as a command from Home
  Assistant, and the answer waits until the schedule task holds them,
  so the page never shows old meals under *Meals saved* (`web::APPLY_WAIT`). The echo moves the
  `Meal n` entities too. A blank time above a set one is refused rather than
  closed up, because closing it renumbers every meal after it.
- **The network form restarts, on top of the record read at that moment**, so
  the knob's last calibration is carried, and an identical record is *Nothing
  changed* with no write and no restart.
- **`configuration_url`** in every discovery config's device block,
  `http://<address>/`, from the address at connect time; left out when there
  is none.

**RAM: nothing new to speak of.** Setup mode's buffers — three connections of
2 KB receive, 2 KB transmit, 2 KB request and an 8 KB page — were always
`StaticCell`s, so they sat reserved in `.bss` on every boot, used or not.
`http::slots()` hands them to whichever server runs, and only one ever does.
The station stack went from 4 to 6 sockets.

Feed, clock and calibration were added the same day and verified with `curl`
for everything that dispenses nothing: a detent saved and restored, a value
off the 10 ms step refused, the clock set from the laptop and read back, 30
February and a zero feed refused. **Feed and Run calibration are not yet seen
turning the motor from the page.**

Verified on the Zero with `curl`: `401` without and with a wrong password;
the page in ~0.1 s, 4 KB; `404`, `405`, and `403` for a foreign `Origin`;
meals saved and echoed on `feeder/<id>/schedule/state` in ~0.2 s; a gap
refused with its message; a hostname refused with the typed value kept; the
unchanged network form answered *Nothing changed* with the unit staying up;
and a real round trip — broker port to 1884, restart, back in ~4 s, port to
1883 from the same page, restart, `online` with meals and calibration intact.
`configuration_url` reads `http://192.168.68.110/` on the broker.

**Point 4 as built.** A meal is a *position* in the schedule, and Home
Assistant shows position *n* as *Meal n time* and *Meal n portions*, reading
both out of the retained `feeder/<id>/schedule/state` echo — so flash stays
the only copy and the entities only ever show it back. An edit is one
non-retained command, `feeder/<id>/meal/<n>/time` or `/portions`, applied by
the schedule task through `Bus::schedule`, now a queue of `ScheduleCommand`s
rather than a `Signal`: a time and a portion count sent a moment apart are two
edits, and a signal would keep only the second. The rules — the first three
host-tested in `schedule.rs`, the last two gated code seen on hardware:

- **Zero portions switches a meal off and keeps it in place.** Removing it
  would renumber every meal after it under the user's feet. A switched-off
  slot is never fed, never counted in `meals`, never the `next` feed — and the
  same now holds for a zero in a whole-schedule command.
- **A time past the end adds the meal switched off**, padding any gap with
  switched-off midnight slots. A time alone never feeds.
- **Portions past the end are refused** (`NoTimeYet`). There is no time to
  feed them at, and inventing one would dispense a meal nobody chose.
- **A refused edit republishes the unchanged echo** (`main.rs`), because neither entity is
  optimistic: that is what makes the value spring back in Home Assistant
  rather than sit showing something the unit never took.
- **A retained edit is refused** (`mqtt.rs`), like a retained schedule command.

Verified on the Zero against the dev stack: all sixteen registered; a time and
portions published back to back both applied, in order, with one flash write
each; portions for meal 6 refused with `NoTimeYet`, `0/time` and `25:00:00`
rejected at the topic; an unchanged value not rewritten; and the recorder
shows `time.cat_feeder_99177c_meal_3_time` going `12:30:00` then `unknown` as
the slot was added and the schedule replaced. Not yet exercised: changing one
from Home Assistant's own UI — the payloads above are the ones its `time` and
`number` platforms send, read from the installed source.

⚠️ A **retained** publish to a command topic *is* acted on by a unit already
subscribed, because the broker forwards it live with the retain flag cleared.
Only the replay to a later subscriber is refused. So the rule stays: never
publish these retained.

**1. The unit owns its schedule, and a new unit starts blank.** Being given a
schedule is an explicit act, not something a unit inherits by connecting. The
design before it had the opposite property — `feeder/schedule` was one shared
retained topic, so a unit that joined was immediately feeding meals nobody chose
for it, and a board on the bench pointed at the house broker would start
turning. A blank unit fails toward not feeding, which is the
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
blanks every feeder with nothing left to republish it. Under the old shared topic
that recovered by itself because Home Assistant republished on restart; with
Home Assistant out of the loop, nothing does. So the schedule goes in the record in `nvs`, beside the
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
server is the other surface — point 5 — which is what Tasmota and Shelly do.

**5. The unit serves its own admin page, in station mode and not only during
setup.** Built on what setup mode already had — request parsing, forms, three
connections — moved into `http.rs` so both servers share it; what was new is
running the listener alongside MQTT, authenticating it, and a page for the
schedule. *Point 5 as built*, in the status paragraph above, has the detail.

It is also the re-provisioning route the button was always a poor substitute
for. *The value is in re-provisioning, not first boot* is already written above,
and "hold the button through power-on, then set it up again from a phone" is a
heavy price for a Wi-Fi password that changed under three units screwed into
place. It adds no boot state: the boot decision stays *no valid record → setup
mode*, and the admin page is one more writer of the same record, so *One way in,
not two* still holds.

Four rules it comes with:

- **The admin password is derived**, `base32(sha256("<ap_secret>:<id>"))`
  — the same string the sticker carries and `./dev/ap-password.sh` prints.
  There is no field to set one, so a unit is never unauthenticated, and
  recovery needs no password-reset flow because a reset gesture forgets the
  network. The physical button stays the root of trust, which is the right answer
  for a device with no other identity.
- **Never render a stored secret back into a form.** The setup page re-fills
  fields on error, and that must not extend to passwords once the page is
  reachable from the house network. Mutations are POST only. It is plaintext
  HTTP on the LAN, like every comparable device — worth stating rather than
  implying otherwise.
- **`configuration_url` in the discovery `device` block** (abbreviated `cu`, and
  accepted by the schema — checked) gives Home Assistant a *Visit device* link
  at whatever address the unit had when it connected, republished on every
  reconnect — `mqtt.rs` re-reads the address at each session, so a new lease
  is picked up. mDNS is not an option: `.local` resolves only over multicast, which
  mesh routers reflect unreliably, and a browser with Secure DNS hands `.local`
  to the upstream resolver and gets NXDOMAIN regardless. The display is the other backstop, and **it
  already shows the address**: turn the knob to the `WI-FI` page — see *The
  screen's pages* below.
- **Saving Wi-Fi or the broker reboots; saving a schedule does not.** With a
  set RTC a restart no longer costs clock trust — it re-arms in about a second
  — but it still costs a few seconds off the broker and would cut short a feed
  in progress, for no reason when only a mealtime changed.

The cost was expected to be RAM and a permanent listener. The RAM turned out
free — see *Point 5 as built* above: setup mode's buffers were already
reserved on every boot. The listener is real: an always-on HTTP server is
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

**Superseded in part.** This used to say the old design stood until v2 and
offered `input_datetime` and `input_number` helpers as the near-term editor,
needing no firmware change. The firmware half of v2 has since landed (points 1,
2, 6 and 7 above), so there is no old design left to extend. Today a person
changes one feeder's times on its own device page, through the `Meal n`
entities, or all three at once by editing `meals` in the package and running
*send the schedule to every feeder*.

Pausing already finds its units rather than being told them — see *Pause stops
the schedule, not the feeder*. That one was cheap enough to do immediately, and
it is what any of these futures wants anyway.

### Timezone: the unit keeps summer time itself

✅ **Built 2026-09-28**, so a feeder with no Home Assistant — or one whose Home
Assistant has stopped — still moves its clock at the summer-time change.
**Home Assistant's live time stays the authority whenever it is there**; the
unit's own zone takes over only after ten minutes without one
(`HA_WINS_MS` in `main.rs`).

**The unit stores a name and a rule, and never turns one into the other.**
`Europe/Rome` is for people; `<+01>-1<+02>,M3.5.0,M10.5.0/3`, the POSIX `TZ`
format, is for the clock. The admin page derives the rule **in the browser**,
from the tz data every browser ships and keeps current, and sends both. So a
country changing its law needs the page opened and *Set* pressed again — the
page says when the browser's rule differs from the stored one — and never a
firmware update. A table of zones compiled into the firmware was the
alternative, rejected because the table would go stale and only a reflash
refreshes it. The knob does not choose zones.

| Piece | Where |
|---|---|
| parsing a rule, the offset at an instant, which offset a local reading is in, following a change | `tz.rs`, pure |
| the stored zone, `FDZ1`, its own sector at nvs+0x2000 | `tz::Zone`, `store.rs` |
| following it each tick, and telling the RTC | `follow_zone` in `main.rs` |
| the list, the rule, *Advanced* | `admin.rs`'s `RULE_JS` and `ZONE_JS`, inline |

**The DS3231 now keeps the offset its time is in**, in alarm 2's registers,
which this firmware never uses as an alarm (`ds3231.rs` has the layout). Local
time alone is ambiguous across a change: a unit off from October to November
would otherwise read its summer reading as winter time and feed an hour out.
A chip written before this reads as *no offset* — local time as it always
was — and is given one on the next write.

**The rule derivation was checked, not just written.** Run in node against
every zone a browser lists: all 418 rules parse in `tz::Rule::parse` and agree
with the browser's own offsets at 24 instants of the year. Two zones are right
for the current year only, because their law is not the kind POSIX rules can
say: Santiago's (the Sunday after the first Saturday) and Casablanca's (around
Ramadan). Home Assistant's live time corrects both, where there is one.

Verified on the Zero with Home Assistant stopped: the zone saved; a running
clock with no offset given `+02:00` and written to the RTC; a hand-set
`2026-03-29T01:59:30` given `+01:00`, then **at 01:00 UTC exactly**
`clock: Europe/Rome changed to 2026-03-29T03:00:00+02:00` and the RTC
rewritten an hour forward; a reboot reading `03:00:52+02:00` back from the
DS3231 with the zone loaded from flash.

### The screen's pages

✅ **Built**, turned by the knob rather than stepped by a locked tap. Six rows
each, the bottom one always saying what a hold does from there.

| Page | Lines |
|---|---|
| home | status banner, last feed, next feed |
| `WI-FI 2/4` | the SSID, this unit's address, whether it is associated |
| `BROKER 3/4` | host:port, username, whether it is connected |
| `DEVICE 4/4` | device id and board, firmware version, detent interval, portion scale |

Home is the old screen rather than *last feed* and *next feed* being pages of
their own: one screen already shows both.

The device page is worth its place because of something already true of the
console: *a feeder behaving oddly is either mis-measured or mis-provisioned and
nothing else tells them apart*, and the calibration used to be printed once at
boot and then only over USB.

Rules, each tested in `display.rs` or `menu.rs`:

- **No page shows a password**, ever. `display::UnitInfo` carries none, so no
  page *can*. Setup mode is a different thing: the password it shows is one the
  unit derived for a network it raised itself.
- **Twenty-one columns is the budget.** `255.255.255.255:65535` is exactly 21,
  so any broker the firmware accepts fits on one line; the unit's own address
  is printed bare, without a scheme.
- **Waking resets to home**, so the first look never lands on a page left over
  from days ago.
- **Pages are locked-only.** While unlocked, turning moves the menu's cursor.

Keep the list short. A screen with a menu is a screen nobody reads to the end,
and everything here is either the feeder's purpose or an answer to *how do I
reach this unit* — which is the question the admin page in (5) makes routine.

The split is unchanged: `button.rs` gains a lock gesture and a page-step event,
`display.rs` gains a page to render, both pure and host-tested, and the task
still only pushes pixels.
