# Roadmap

Open work for the project as a whole. Progress on any particular set of units
belongs with whoever is building them, not here. When an item is done, delete
its row; git history keeps the record.

## Not built

| Item | Notes |
|---|---|
| OTA updates | built and seen working on a Zero (ADR-0022, ADR-0024): `dev/ota.sh`. What is left is below |
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
  (ADR-0030) is the half of it already done, and `dev/kicad-netlist.sh`
  already writes it as a KiCad netlist, with footprints for the modules.
- Tune the LED palette in a real room; green reads much brighter than blue at the same value.
- A boot reaching an armed schedule should take about 6 s; not captured end to end.
