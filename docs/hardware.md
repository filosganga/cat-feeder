# Building the hardware

The builder's guide: what goes in the feeder, what goes on the board, and
what to check before power goes anywhere near it. The README has the parts
list and the firmware side; this page is the wiring.

Every pin number the firmware uses lives in [`src/board.rs`](../src/board.rs),
which carries the full assignment table. Numbers appear below only where a
wiring step needs one.

## What the firmware needs from the feeder

The original board (LCD, clock, buttons) comes out. The mechanics stay:

- **A 5 V DC geared motor** turning the dispensing hub. The reduction gearbox
  matters: the hub stops dead when the motor brakes, with no coasting past a
  detent.
- **A microswitch on the output hub** that closes once per detent. **One click
  is one portion.** That is the entire contract between the firmware and the
  mechanism.

The firmware reads the switch with the chip's internal pull-up, the other
contact to ground, and counts **falling** edges. An optical or Hall-effect
sensor is a different shape entirely and will not work without firmware
changes.

### Three figures that matter

| Figure | What breaks if it differs |
|---|---|
| 1 click = 1 portion | every portion count, from the Home Assistant button to each meal |
| the detent interval | the minimum click spacing (0.4×) and the jam timeout (2.5×) are both derived from it |
| a microswitch on the hub at all | the firmware assumes a pull-up and a falling edge |

Clicks per revolution is **not** on that list: nothing counts revolutions. A
full turn giving the *same* count every time is still a useful bench check,
because it catches clicks being missed or doubled.

The detent interval is set per unit, in flash, and measured by the unit
itself — *Run calibration* on its admin page. **Measure it with a full
hopper.** A loaded mechanism turns slower; calibrating on an empty one sizes
the jam timeout on the fastest case, and the first refill brings a false jam:
motor stopped, meal dropped, solid red. Calibrating on the slow case is safe
in both directions. Record the empty figure too if you can — the ratio says
whether 2.5× is enough margin for that mechanism.

The portion size is the other per-unit figure: if one mechanism dispenses more
per click than another, set its portion scale (100% is unchanged, 133% turns
three portions into four clicks). Measure it by weight, or by counting clicks
into a measuring spoon.

### Two known mechanisms

Two brands are known to work. They look alike outside and are the same design
inside — a geared motor and a hub microswitch — with different parts. Feeder B's
switch clicks four times per full turn.

| | Feeder A | Feeder B |
|---|---|---|
| Motor | DRF-W500CA, 5 V, 8 rpm | HC 180-15180, 5 V — a 180-size motor in its own reduction gearbox |
| Detent interval | about 2 s (≈2050 ms measured with an empty hopper; the firmware's default is 1900 ms) | about 5.3 s, empty hopper, measured with 4.7 V across the motor — the mechanism is slow, not the supply |
| Wiring out of the mechanism | separate wires: 5 V, Vbatt, GND, switch, motor 1, motor 2 | a **flat ribbon cable**: GND, 5 V, Vbatt, motor 1, motor 2, switch, then the loudspeaker's conductors |
| Suppression capacitor on the motor | none fitted | none fitted |

Neither has shown spurious clicks without a suppression capacitor. If yours
does, add 100 nF across the motor terminals.

Full-hopper figures for both are not measured yet, so provision with your own.
The firmware accepts any detent interval from 200 to 10 000 ms.

The motor is back-drivable by hand, but the gearbox is stiff enough that
turning the hub by hand is no way to measure anything. Let the motor do it.

The feeder's loudspeaker (often a small cone with a record button on the
front) is not used. It is a speaker, not a buzzer, and cannot be driven from a
GPIO.

## Parts, per feeder

| Part | Notes |
|---|---|
| Waveshare ESP32-C6-Zero | the production board. 8 MB flash — `espflash board-info` reports 8 MB, though the schematic's `ESP32-C6FH4` suggests 4 MB. The chip is the authority |
| DRV8833 breakout (black, 10-pin) | H-bridge. `nSLEEP` (labelled `ULT` or `SLP`) **must be driven high** or the motor will not turn; the firmware does that on GPIO14 |
| DS3231 module | real-time clock, with a **LIR2032** rechargeable coin cell — see below |
| 220 µF 16 V electrolytic | across the rail after the diode-OR. Without it the motor's inrush browns out the ESP32. Its two legs are the star points for 5 V and ground |
| MBRF2045CT dual common-cathode Schottky | the diode-OR between USB and the feeder's batteries. A part marked `MBRF2045DT` is a vendor variant and works |
| 1 kΩ resistor | R1, in series on the switch line |
| perfboard, female headers, a 6-pin header for the feeder's cable | the layout is [`pcb.diy`](../hardware/pcb.diy), double-sided and plated-through |
| JST XH 6-pin header and housing | optional, for a feeder whose wires need re-crimping anyway |
| 2× 1 kΩ resistor, a 2-pin female header, a JST XH 4-pin header | optional: R2, R3, `Pads` and `Ext`, for the LED and BOOT board — see *The LED and BOOT connectors* |
| Waveshare ESP32-C6-DEV-KIT-N8 | optional, for development on a breadboard |

Power comes from the feeder's own USB port (a 1 A adapter or better). The
feeder's AA compartment can stay in use as a backup supply.

## Power

### USB and batteries: the diode-OR

The board takes 5 V from the feeder's USB socket and 4.5 V from its three AA
cells, and joins them through the MBRF2045CT: anodes on the two supplies,
the common cathode (the middle leg) on the rail. Whichever supply is higher
feeds the board, and neither can push current into the other — so USB never
charges the alkaline cells. **A plain wire from the battery pin to the rail
would.**

The rail sits a Schottky drop below the higher input: about 4.7 V on USB,
falling with the cells on batteries. The Zero's regulator tolerates down to
about 3.5 V.

On batteries — **estimated, not measured** — the C6 on Wi-Fi draws 80–120 mA,
so three AAs last roughly a day. It is a bridge over a power cut, not a way to
run. The motor turns slower on cells; the 2.5× jam budget is meant to absorb
that. The firmware cannot tell which supply it is on.

### A laptop on USB at the same time

On the bench a feeder can have its own adapter *and* a laptop on the Zero's
USB-C. That is safe: the Zero carries a B5819WS Schottky (`D1`) between the
USB connector's `VBUS` and its `5V` pad, conducting only from USB into the
board, so the pad can never push current back into the laptop. **Do not fit
an external diode for this; there is already one.**

About 4.8 V measured on the Zero's `5V` pad from USB alone is that diode's
drop, not a failing supply.

With the diode-OR, the adapter reaches the rail a Schottky drop lower, so the
laptop (through `D1`) and the adapter (through the MBRF) sit at about the same
voltage and **share the rail, motor current included**. That follows from the
two parts' drops and has not been measured. It is harmless for a running
motor.

> ⚠️ **Never provoke a jam on USB alone, and unplug the laptop before testing
> one.** A stalled motor held for the whole jam budget draws more than the
> Zero's 1 A diode should carry. Test a jam on the adapter, with the laptop
> disconnected.

### Ground

All grounds are one node — adapter, driver, Zero, RTC. The DRV8833's inputs
are referenced to the Zero's ground, so they must be.

## The perfboard

[`pcb.diy`](../hardware/pcb.diy) is the layout, drawn in
[DIY Layout Creator](https://github.com/bancika/diy-layout-creator). Open it
there to print or export a picture to solder from. What it must connect is
[`hardware/netlist.toml`](../hardware/netlist.toml); the drawing places it. GPIO3–GPIO5 are
unconnected, and the DS3231 is alone on the I²C bus.

**The drawing is the component side**, with the bottom traces seen through the
board. Columns run `A … X` left to right and rows 1–18 bottom to top, matching
the board's printed labels from the component side. Every hole below is named
by those labels, which name the same hole from either side — so count by label,
never by "third from the left", which flips when the board is turned over.

**Modules sit in female headers**, not soldered, so a mistake costs the
perfboard rather than the Zero, the driver and the RTC. Socketed modules can
go in reversed or one row off: mark each socket's GND end on the board.

The Zero's own LED and BOOT button are the only ones on the board. Two
optional connectors, `Pads` and `Ext`, take both to a small second board that
can sit where they are seen and pressed from outside the feeder; whether a
feeder gets one is not decided (ADR-0029). See *The LED and BOOT connectors*.

### The feeder's cable

One 6-pin header along the bottom edge, `F1 … A1`, in this order:

| Pin | Hole | Signal | Goes to |
|---|---|---|---|
| 1 | F1 | GND | C1 −, the ground star point |
| 2 | E1 | 5 V from the feeder's USB | MBRF2045CT, one anode |
| 3 | D1 | Vbatt, 4.5 V from the feeder's batteries | MBRF2045CT, the other anode |
| 4 | C1 | motor 1 | DRV8833 `Out1` |
| 5 | B1 | motor 2 | DRV8833 `Out2` |
| 6 | A1 | hub microswitch | R1 (1 kΩ), then GPIO2 |

This is the ribbon-cable feeder's order, so its ribbon plugs straight in. The
ribbon's conductors after the switch belong to the loudspeaker; leave them
unconnected.

**A feeder with separate wires** (5 V, Vbatt, GND, switch, motor 1, motor 2)
needs its cable re-crimped into the order above. Use a **JST XH** 6-pin header
in `F1 … A1` and crimp the feeder's wires into an XH housing. XH is 2.5 mm
pitch, close enough to the board's 2.54 mm to sit in a row (about 0.2 mm over
six pins). **JST PH is 2.0 mm and does not fit.** XH is keyed, so it cannot go
in reversed — but a housing crimped in the wrong order is something no key
catches, so beep the cable end to end before plugging it in.

**Check the switch returns to ground.** A cable with one switch conductor
means the switch's other leg is ground inside the feeder. Before fitting,
beep pin 6 ↔ pin 1 while the motor turns the hub: it must close once per
detent. A switch returning to 5 V instead would put 5 V on GPIO2.

If the motor turns the wrong way, swap pins 4 and 5. No firmware change.

### The driver

The DRV8833 runs on channel A: GPIO0 → `In1`, GPIO1 → `In2`, GPIO14 → `Ult`
(`nSLEEP`), `Out1`/`Out2` to the motor. The firmware does not care which
channel, as long as the pins in `board.rs` reach it.

| IN1 | IN2 | |
|---|---|---|
| 1 | 0 | forward (feed) |
| 0 | 0 | coast |
| 1 | 1 | brake |

Feeding runs forward until the requested number of falling edges, then brakes
**on** the last one, so the hub always parks in the same place. No click
within the jam budget stops the motor and reports a jam. There is no reverse,
deliberately: reversing could jam the mechanism against its own geometry.

> ⚠️ **The driver module's middle pins read `Vcc` then `GND`** after
> `In1`/`In2`. Swap them and the DRV8833's supply is reversed: current flows
> straight through its protection diodes, the laptop's port cuts power, and
> the Zero never enumerates — plugged in, nothing happens. Check the module's
> silkscreen against H13/H14 before soldering.

### The real-time clock

The DS3231 uses its **4-pin passthrough side**, `SCL · SDA · VCC · GND` on
J6 … J3. The 6-pin side has the same signals in mirrored order, so fitting the
wrong side lands every pin on its mirror: nothing is damaged, and nothing
answers.

One open I²C line and one wrong pin order look identical on the console —
`rtc: nothing answered; running without one` — so beep each module pin to the
Zero's: SDA J5 ↔ S10 (GPIO18), SCL J6 ↔ R10 (GPIO19).

> ⚠️ **Power the DS3231 from 3V3, never 5 V, and fit a LIR2032.** The common
> DS3231 breakouts charge their coin cell from `VCC`. That suits a
> rechargeable LIR2032 at 3.3 V and overcharges it at 5 V. An ordinary CR2032
> is a primary cell and must not be charged at all: to use one, lift the
> module's series charging resistor or diode first. The symptom of getting
> this wrong is a cell flat within months, which looks like a bad module.

The DS3231 is chosen for its oscillator-stop flag rather than its accuracy:
it can say *I lost power and the time is not real*, so a unit with a flat cell
waits for a real time instead of feeding on a plausible wrong one.

It answers on I²C address `0x68`.

### Star grounding on C1

5 V and ground are starred on the 220 µF capacitor's legs (E5 `+`, G5 `−`),
not chained along a rail. The cable's ground reaches `−` before anything
leaves it. Then:

- **The DRV8833 gets its own 5 V and ground run to C1**, because the motor's
  current flows through it. Shared with the Zero, the return current would
  shift the ground GPIO2 is read against, and the start-up dip would reach the
  Zero's supply.
- **The Zero gets its own pair**, and the DS3231 hangs off the Zero's ground
  and 3V3. At tens of milliamps a shared run is harmless.
- **The external LED hangs off the driver's pair** (its `Vcc` and `GND` pins,
  H13 and H14), the only 5 V and ground that reach the `Ext` connector without
  crossing another run. One WS2812 draws 60 mA at most, beside the motor's
  amperes, and nothing reads a voltage against it: a BOOT press is still a
  clear low with the motor running.

### R1, the guard on the switch line

On an unkeyed header the switch pin sits beside motor 2, and the switch's run
along row 17 passes the 5 V run on row 18. A plug reversed or one position
off, or a solder blob between those rows, would put 5 V on GPIO2. **R1, 1 kΩ
in series at O17–R17**, limits that to a current the pin's clamp diode
survives, and changes nothing about reading a switch that pulls to ground.
Keep it fitted with a keyed connector too.

It sits at the GPIO2 end so the whole row-17 run, D17 to O17, is behind it.
Only R17 (joined to R16) is unguarded beside 5 V — one pair, R17|R18, which is
what the beep test's GPIO2 ↔ 5 V line checks.

A 100 nF capacitor from GPIO2 to ground is not fitted: the 30 ms debounce and
the click-spacing rule cope with the noise a switch wire picks up in the
motor's cable. Fit one only if spurious clicks appear.

Mark pin 1 on the housing and on the board either way.

### The LED and BOOT connectors

Optional: fitted for a feeder whose LED and BOOT are brought outside on a
small second board, left empty otherwise (ADR-0029). Both signals already
exist. GPIO8 drives the onboard WS2812 and GPIO9 is the onboard BOOT button,
and both are also pads on the back of the Zero (test points `P2` and `P4` on
its schematic, closer together than 2.54 mm). An external WS2812 on GPIO8
latches the same data and shows the same colour; a button from GPIO9 to
ground sits in parallel with BOOT. No firmware change and no other pin.

- **`Pads`, K10 (GPIO8) and L10 (GPIO9)**: a 2-pin female header. Two wires
  soldered to the back pads run out from under the Zero to a 2-pin male
  Dupont plug. The drawing knows the Zero only by its edge headers, so this
  is where the two pads enter it. Plugged in swapped, nothing is harmed: the
  LED and the button just do nothing.
- **R2 and R3, 1 kΩ**: GPIO8 through R2 (J10–J14), GPIO9 through R3 (O11–O15,
  under the Zero). The same guard as R1: a fault on the cable or the small
  board that reaches 5 V meets 1 kΩ before it meets a pin. On the data line it
  costs tens of nanoseconds of edge, against 220 ns for the shortest WS2812
  pulse.
- **`Ext`, J16–M16**: a JST XH 4-pin header, keyed.

| Pin | Hole | Signal | Goes to |
|---|---|---|---|
| 1 | J16 | GND | the DRV8833's `GND` (H14) |
| 2 | K16 | 5 V | the DRV8833's `Vcc` (H13) |
| 3 | L16 | DIN, the external WS2812's data in | R2, then GPIO8 |
| 4 | M16 | BOOT, the external button | R3, then GPIO9 |

5 V sits between ground and DIN, never beside a bare pin: DIN is behind R2.
The XH's body is wider than a Dupont header's, so columns I and N beside it
carry traces only, no parts.

**The small board**: a WS2812 on 5 V with 100 nF across its supply, a push
button from BOOT to ground, and a JST XH 4-pin header in the order above. The
key stops a reversed plug, but not a housing crimped in the wrong order: beep
the cable end to end before plugging it in.

**At power-on.** Both lines are strapping pins. GPIO9 low at reset means
download mode, so holding the external button through a reset does what
holding BOOT does; released, the Zero's own 10 kΩ pull-up holds it high, cable
or no cable. GPIO8 matters only while GPIO9 is low, has a 10 kΩ pull-up of its
own, and an external DIN loads it no more than the onboard LED's does. **What
does harm is a fault holding BOOT low**: held 5 s while the unit runs, it
forgets the network and reboots — into download mode, if BOOT is still low.
That is why the beep test checks BOOT against everything beside it.

**3.3 V data into a 5 V LED.** GPIO8 drives 3.3 V. Older WS2812B datasheets
want 0.7 × VDD for a high, which on a 4.7 V rail is 3.3 V, no margin at all;
newer ones (V5, V6) want 0.55–0.63 × VDD and are comfortable. With an older
part, a 1N4148 in series with the LED's 5 V puts it at about 4.0 V and its
threshold at about 2.8 V.

## Before soldering: `./dev/pcb-check`

Run it after any edit to `pcb.diy`. It reads the drawing and fails on:

- a header whose pin names disagree with the part's silkscreen
  ([`hardware/pinouts.toml`](../hardware/pinouts.toml));
- a supply joined to ground or to another supply;
- a net that differs from [`hardware/netlist.toml`](../hardware/netlist.toml), the
  statement of what connects to what (ADR-0030): split, joined to another, or
  reaching a pin meant to be spare;
- a GPIO wired other than where `src/board.rs` drives it;
- a label on the wrong pin.

`./dev/pcb-render` draws what it reads to `target/pcb.svg`, each net named
by the netlist, for a look without opening DIY Layout Creator. To solder from:
`--side solder` draws the bottom as the board is held to solder it, mirrored,
and `--true-size` prints at 100% with the holes 2.54 mm apart.

It also ranks every pair of neighbouring holes on different nets, worst first.
Those are where a solder blob does damage, and where the care goes:

- 5 V (F8–F14) beside ground (G7–G12) on the way to the driver;
- `Out1` (D9–D15) beside 5 V (E9–E18);
- the 3V3 run beside `In1` at I10/I11;
- rows 17 and 18 (the switch line beside 5 V);
- 5 V (I14, J15, K16) beside the LED's data line (J14, K15, L16), behind R2;
- BOOT (M16, N16, O15) beside the switch line, DIN and `In2`: no part is
  damaged, but a bridge holds BOOT low and the unit forgets its network.

It cannot see a part fitted backwards, a cold joint, or a module in the wrong
socket. That is what the beep test is for.

## Before first power: the beep test

With the continuity beeper, modules plugged in, holes as in the drawing:

| Probes | Expect | Catches |
|---|---|---|
| E5 ↔ G5 (C1 + ↔ −) | silence | 5 V shorted to ground |
| W16 ↔ V16 (Zero 5V ↔ GND) | silence | the same, at the Zero |
| H13 ↔ H14 (DRV Vcc ↔ GND) | silence | the same at the driver — and a shorted driver |
| U16 ↔ V16 (Zero 3V3 ↔ GND) | silence | 3V3 shorted to ground |
| E5 ↔ U16 (5 V ↔ 3V3) | silence | 5 V on the 3V3 rail |
| F3 ↔ D3 (USB ↔ battery anodes) | silence | a bridge that charges the cells |
| A1 ↔ B1 (switch ↔ motor 2) | silence | a bridge at the header |
| R16 ↔ W16 (GPIO2 ↔ 5 V) | silence | R17 touching R18, the one GPIO2 hole beside 5 V |
| A1 ↔ R16 (switch ↔ GPIO2) | ~1 kΩ on the ohms range | R1, the D17–F17 jumper and the R17–R16 link in place |
| H11 ↔ U16 (`In1` ↔ 3V3) | silence | the 3V3 run beside `In1` — a bridge runs the motor on its own |
| T16 ↔ H11, S16 ↔ H12, U10 ↔ B11 (GPIO0 ↔ `In1`, GPIO1 ↔ `In2`, GPIO14 ↔ `Ult`) | beep | the runs and jumpers reaching the driver |
| J5 ↔ S10, J6 ↔ R10 (SDA ↔ GPIO18, SCL ↔ GPIO19) | beep | the RTC's lines, module pin to Zero pin |
| *With `Pads` and `Ext` fitted:* | | |
| M16 ↔ J16, L16, A1, H12 (BOOT ↔ GND, DIN, switch, `In2`) | silence | a bridge that holds BOOT low and forgets the network |
| L16 ↔ K16 (DIN ↔ 5 V) | silence | 5 V on the data line's side of R2 |
| K10 ↔ L16, L10 ↔ M16 (GPIO8 ↔ DIN, GPIO9 ↔ BOOT) | ~1 kΩ on the ohms range | R2 and R3 and their runs in place |
| J16 ↔ V16, K16 ↔ W16 (`Ext` GND, 5 V ↔ the Zero's) | beep | the rails reaching `Ext` |

Then, on USB alone:

1. About 4.7 V across C1.
2. 3.3 V on the Zero's `3V3`.
3. `./dev/flash --board zero` shows `switch: watching GPIO2`,
   the RTC answering, and the LED's red-green-blue sweep.

Before the unit goes into its feeder, **check the motor's direction** with a
feed. Finding out afterwards means taking it apart again.

A factory-fresh Zero arrives running Waveshare's demo, which leaves data in
the `nvs` partition. The console then says `schedule: stored record is
unreadable` until the unit is given a schedule. That is expected.

## Finishing and fitting

**Insulating spray** on the solder side, **after** the beep test and a working
power-up — it seals a bridge in rather than fixing it — and kept out of the
header's contacts.

The board goes **inside the feeder**, where its original board was
(ADR-0029). No separate case, and no cable leaves the feeder.

- **The LED and BOOT**: whether either is brought outside is not decided
  (ADR-0029). The LED can show through the old display window, a button hole
  or a hole of its own, or a light pipe; it is the only output the unit has,
  and the only one left when the network is what failed. BOOT can be reached
  through a hole. Or both go out together on the small board on `Ext` (*The
  LED and BOOT connectors*), fixed in the feeder's wall with the cable inside.
  Without any of these, both need the feeder open.
- Stand the solder side off whatever it is fixed to — about 3 mm, on the
  corner mounting holes — so clipped leads touch nothing.
- A drop of glue or a cable tie takes the cable's pull off the header's joints.
- The USB-C socket and the RTC's coin cell are reached by opening the feeder.

## Pins

[`src/board.rs`](../src/board.rs) is the single source for every pin, for both
boards. In summary, on the Zero:

| Function | Pin |
|---|---|
| DRV8833 `In1`, `In2`, `nSLEEP` | GPIO0, GPIO1, GPIO14 |
| hub microswitch | GPIO2 |
| onboard WS2812 LED | GPIO8 |
| BOOT button | GPIO9 |
| I²C `SDA`, `SCL` (the RTC) | GPIO18, GPIO19 |

Every switch input uses the chip's internal pull-up and reads a press as a
falling edge; none needs an external pull-up.

What constrains the choice, if you move anything:

- The Zero does **not** bring out GPIO10 or GPIO11 at all.
- GPIO8, GPIO9 and GPIO15 are strapping pins that matter (boot mode, boot log,
  JTAG source). GPIO8 is also the onboard LED; GPIO9 is the BOOT button.
- GPIO12 and GPIO13 are native USB, the Zero's only console.
- GPIO4 and GPIO5 are strapping pins too, but on the C6 they only set SDIO
  sampling edges, so a part holding them through reset cannot stop the unit
  booting.
- Peripheral signals route through the C6's GPIO matrix, so any free pin can
  carry I²C or anything else. GP3–GP7 and GP20–GP23 are free.

## An external LED

On the Zero, GPIO8 is also a back pad. An external WS2812 wired there sits in
parallel with the onboard one on the same data line and shows the same
colour — an indicator where the feeder can show it, such as its old display
window, for no extra pin and no firmware change. On the perfboard it arrives
through `Pads` and leaves on `Ext`, at 5 V; *The LED and BOOT connectors* has
the wiring and the 3.3 V data margin. Both boards' LEDs take RGB byte order,
not the GRB the WS2812B datasheet gives; if an external part shows the wrong
colours, `led::wire_word` is the one place to change.

## Developing on a breadboard

The ESP32-C6-DEV-KIT-N8 runs the same firmware (`--board devkit`, the
default) with the same pin numbers. Its console is a USB-serial bridge rather
than the chip's own USB, so it enumerates as a different port.

> ⚠️ On the dev kit's J1 header, which reads `5V · GPIO3 · GPIO2 · GPIO11`,
> the nearest ground (J1 pin 15) and GPIO3 both sit **directly beside 5V**. A
> jumper one position off puts 5 V on a signal pin and destroys it. Take
> ground from the J3 header, which has no 5 V neighbour.

When wiring a push button as the hub switch on a breadboard, put its GPIO leg
and its ground leg in **different** row groups. In the same group the pin is
grounded permanently and the switch is bypassed. The console prints the
switch's level at boot: `currently pressed` on an untouched button is the
whole diagnosis.
