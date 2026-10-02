# OTA is two app slots, pushed to the admin page, confirmed by the broker

Amended by [ADR-0024](0024-our-own-bootloader-rolls-back.md): the bootloader, not
the image, is what goes back to the old slot.

A unit closed inside a feeder can only be reflashed by opening it, so it updates
itself: the ESP-IDF scheme `esp-bootloader-esp-idf` already implements. The
running image writes the other slot, points `otadata` at it, and reboots; a
power cut at any step leaves the old image selected.

- **One partition table, fitting 4 MB.** `nvs` stays at 0x9000 / 24 KB, so
  every record survives the change; `otadata` at 0x10000, `ota_0` at 0x20000
  and `ota_1` at 0x200000, 0x1E0000 each. No `factory` slot. Moving a unit to
  it is one last USB flash, and every USB flash erases `otadata`, or the
  bootloader keeps booting the slot it names rather than the one just written.
- **Pushed, not pulled.** `POST /update` on the admin page, behind its auth and
  `Origin` check (ADR-0017), with the image from `espflash save-image`. No
  HTTP client and no TLS on the unit; Home Assistant's `update` entity can be
  added later on top of the same write path.
- **Confirmed by the broker.** A new image stays unconfirmed until a CONNACK;
  without one within a few minutes it marks itself invalid and boots the old
  slot. Feeding works throughout, so the cost of a bad image is minutes
  offline, never a missed meal.
- **Never while feeding.** Flash writes stall code running from flash, so the
  upload is refused mid-turn and feed requests wait for the write to finish.
- **Same `ap_secret`.** The image carries it (ADR-0004); one built from a
  different `cfg.toml` would change the unit's passwords, so the upload
  refuses an image whose secret differs.

Rejected: firmware over MQTT (a payload size the broker and HA must agree on,
for no gain on a LAN), and signed images (secure boot burns eFuses; the LAN and
the admin password are the boundary, as for the admin page).
