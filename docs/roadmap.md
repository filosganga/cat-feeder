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

## To measure, per feeder model

| Figure | Why |
|---|---|
| What one click dispenses, against the other models | sets each unit's portion scale (ADR-0007) |
| Motor direction on the real mechanism, before closing it up | if wrong, swap the two motor pins at the header |

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

## Dev tooling

Each item has the prompt to start a new chat with.

### Capture that shows the console live and keeps it however it ends

`dev/capture` shows nothing until `--seconds` (45 by default) have passed,
then renders the whole log at once; stopping it early with Ctrl-C loses the
output, so the only way to see anything is to wait for the end.

```text
Make dev/capture show the serial console live while it records, and keep
the record however the capture ends.

Today it writes the port to a mktemp log through dev/_tap.py (or espflash
monitor with --reset), shows nothing until --seconds (45 by default) have
passed, then renders the log with dev/_render.sh. Ctrl-C before the end loses
the rendered output, so the only way to see anything is to wait.

Wanted:
- lines appear as they arrive, rendered as _render.sh renders them: colour
  codes stripped, the +N ms gap since the previous line, --filter applied;
- a way to run until Ctrl-C (--seconds 0 or a --follow flag: pick one and say
  why);
- Ctrl-C ends the capture cleanly and still prints the summary (application
  line count, PANIC detection, the "printed nothing while listening" hint),
  and the log is kept at a path it prints, so a session can be read back.

Keep: listening without a reset by default (a reset before an OTA image
confirms itself rolls it back, ADR-0024), waiting for the Zero's USB port to
come back after the unit resets, --reset through espflash monitor, the
existing positional arguments, and dev/flash, which shares _render.sh,
rendering as it does now.

Follow the dev-script skill, and update the flash-and-verify skill if what
the console shows changes. Verify on the bench Zero: a live capture shows boot
lines as they come; Ctrl-C mid-capture prints the summary and leaves the log;
--filter applies live; dev/flash's output is unchanged. Branch first; don't
commit until I say.
```
