# Roadmap

Open work for the project as a whole. Progress on any particular set of units
belongs with whoever is building them, not here. When an item is done, delete
its row; git history keeps the record.

## Not built

| Item | Notes |
|---|---|
| OTA updates | built and seen working on a Zero (ADR-0022, ADR-0024): `dev/ota`. What is left is below |
| An external WS2812 on GPIO8, and BOOT beside it | no firmware change: in parallel with the onboard LED and button. The perfboard has the connectors (`Pads`, `Ext`); the small board is not drawn and its 5 V LED not chosen |

### OTA, in order

1. **Move each unit to the table and the bootloader** with one USB flash;
   after that, updates go over the network.

## Built, not yet seen on hardware

| Item | How to see it |
|---|---|
| Setup's *Save anyway*, and the captive portal on iOS | a phone on a unit in setup: a broker that refuses, then *Save anyway*; an iPhone joining from the label |
| The factory reset on the admin page and the setup page (ADR-0027) | tick, confirm: `web: factory reset` or `setup: factory reset`, then setup mode with no `keeping this unit's measured timings`; see *Factory reset* in the `flash-and-verify` skill's serial expectations |
| *Feed now*, *Run calibration*, *Use this device's time*, the timezone list, and a phone layout on the admin page | use it from a phone |
| Changing a `Meal n` entity from Home Assistant's own UI | built against the payloads HA sends; not driven from the UI |
| `script.cat_feeder_copy_schedule` to a label, an area or a floor | run it so |

## Later

- **The board in KiCad**, if a perfboard stops being enough: a schematic with
  ERC, the layout on a 2.54 mm grid, DRC including body outlines and 3D fit
  (`pcb-check` cannot see a part's size), all of it headless through
  `kicad-cli`, and the same project for a manufactured PCB. It means redrawing
  `pcb.diy` and retiring `pcb-check` and `pcb-render`. `dev/netlist.toml`
  (ADR-0030) is the half of it already done, and `dev/kicad-netlist`
  already writes it as a KiCad netlist, with footprints for the modules.
- Tune the LED palette in a real room; green reads much brighter than blue at the same value.
- A boot reaching an armed schedule should take about 6 s; not captured end to end.
- **Knowing it runs on batteries, and sleeping on them.** Planned, not worth
  building yet: batteries are a bridge over a power cut (ADR-0013). The
  plan is below.

### Batteries: the plan

The firmware cannot tell which supply it is on (ADR-0013), and on batteries
it keeps Wi-Fi up, so three AAs last about a day. The plan: sense the supply,
then on batteries drop Wi-Fi and deep-sleep between meals. In a power cut the
router is usually down too, so the radio would mostly be spent reconnecting.

The perfboard, wired before a board is soldered even if the firmware comes
later. Every pin that wakes the chip or measures is a low-power GPIO
(GPIO0–7):

| Pin | Goes to | Notes |
|---|---|---|
| GPIO0 | DS3231 `32K` | `XTAL_32K_P`, the C6's only external slow-clock input: a sleep timer as good as the DS3231. esp-hal 1.1.2 leaves that source `todo!()`; until it lands, a boot check counts edges (~328 in 10 ms) and `EN32kHz` stays off otherwise, since the output's pull-up draws while it runs |
| GPIO1 | the MBRF's USB anode, ÷3, **stiff** (20 kΩ / 10 kΩ) | high = on USB, and the wake when USB returns. A light divider would let the Schottky's reverse leakage float a dead anode up to "present" |
| GPIO4 | the MBRF's battery anode, ÷3, light (200 kΩ / 100 kΩ, 100 nF across the lower) | read on USB too, so a flat backup shows before the cut. Below ~2.5 V means no cells: the same leakage |
| GPIO5 | DS3231 `SQW`, alarm 1 | open-drain, pulled up to 3V3; wakes the chip low at the next meal. Alarm 2 holds the offset (ADR-0016) |
| GPIO20, GPIO21 | DRV8833 `IN1`, `IN2` | moved off GPIO0/1 |

The DS3231 stays flat on its 4-pin side; standing it on the 6-pin side is too
tall for the feeder. `32K` and `SQW` come off the 6-pin side on two wires to a
2-pin plug, a pseudo-module like `Pads`. `pcb-check` wants a `board.rs` macro
for every net that reaches a GPIO, so each new one gets its macro with the
wiring. A laptop on the Zero's USB-C feeds the rail past the USB sense, so the
bench reads "on batteries".

In order:

1. **Measure the battery current**: as the firmware runs today, and in deep
   sleep. The board's floor (the WS2812 idling, the DS3231 module, the Zero's
   regulator), not the chip's ~10 µA, decides whether this buys weeks or
   months. The Zero browns out near 3.5 V on the rail, about 1.27 V per cell
   after the Schottky, so a good part of each cell is never used.
2. **Read ADC1 with Wi-Fi up**, on the dev kit: on the C6 the Wi-Fi driver
   takes the ADC as its entropy source (`TrngSource`), and one-shot reads
   alongside it are unproven.
3. **Sense** (`supply.rs`, core): divider ratio, averaging, hysteresis, no
   cells, samples taken while the motor turns ignored. `supply` and
   `battery_mv` in `feeder/<id>/state`, a binary sensor and a voltage sensor.
   A new ADR superseding ADR-0013's "no supply detection".
4. **Sleep.** A deep-sleep wake is a reboot, and the baseline pass would mark
   the meal it woke for as consumed without feeding: the consumed marker has
   to survive in LP memory, or the unit light-sleeps instead. Clear alarm 1's
   flag on every boot, or the pin holds the chip awake.
