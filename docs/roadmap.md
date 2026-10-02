# Roadmap

Open work for the project as a whole. Progress on any particular set of units
belongs with whoever is building them, not here. When an item is done, delete
its row; git history keeps the record.

## Not built

| Item | Notes |
|---|---|
| Schedule editor on the knob | meals arrive over MQTT or the admin page only |
| Wi-Fi and broker entry on the knob | needs a character picker; parked |
| A short press feeding one portion with the broker down | would revisit ADR-0010 |
| OTA updates | designed (ADR-0022, ADR-0024); the partition table and the rollback bootloader are in and seen working on hardware. Steps below |
| An external WS2812 on GPIO8 | no firmware change: it sits in parallel with the onboard LED |
| The printed enclosure | one design for every feeder (ADR-0012) |

### OTA, in order

1. **A watchdog.** `esp_hal::init` disables every watchdog and the panic
   handler spins forever, so today a panic stops the unit, possibly with the
   motor running. A hang must become a reset, which is also what hands a bad
   image back to the bootloader (ADR-0024).
2. **Firmware:** mark the running image valid on CONNACK; without one after a
   timeout (its value from a measured cold boot to the broker), reset and let
   the bootloader roll back; `POST /update` in `web.rs` writing the other slot through
   `OtaUpdater`; refused while feeding, feeds held during the write; refuse an
   image whose `ap_secret` differs (how the image carries it is open, e.g. a
   hash in the app descriptor).
3. **`dev/ota.sh`** (`--host`, `--password-file -`): `espflash save-image`,
   then `curl` it to the unit.
4. **Move each unit to the table and the bootloader** with one USB flash;
   after that, updates go over the network.

## Built, not yet seen on hardware

| Item | How to see it |
|---|---|
| The setup-mode screen (SSID, password, address) | hold the knob's click through power-on, `./dev/capture.sh --seconds 40` |
| The `WI-FI`, `BROKER` and `DEVICE` info pages | turn the knob while locked |
| The BOOT hold's `HOLD TO ERASE WI-FI` banner on a unit with a panel | hold BOOT on a knob unit, then re-provision |
| *Feed now*, *Run calibration*, *Use this device's time*, the timezone list, and a phone layout on the admin page | use it from a phone |
| Changing a `Meal n` entity from Home Assistant's own UI | built against the payloads HA sends; not driven from the UI |
| `script.cat_feeder_copy_schedule` to a label, an area or a floor | run it so |

## To measure, per feeder model

| Figure | Why |
|---|---|
| Detent interval with a **full** hopper | the jam budget is derived from it (ADR-0006) |
| What one click dispenses, against the other models | sets each unit's portion scale (ADR-0007) |
| Motor direction on the real mechanism, before closing it up | if wrong, swap the two motor pins at the header |

## Later

- Tune the LED palette in a real room; green reads much brighter than blue at the same value.
- A boot reaching an armed schedule should take about 6 s; not captured end to end.
