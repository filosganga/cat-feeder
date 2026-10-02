# The unit boots our own bootloader, and it is what rolls back

ADR-0022 needs a way back from a new image that never confirms itself.
espflash's bundled bootloader has no rollback: an image left `New` in
`otadata` is booted on every reset, forever. So the repo carries its own,
`bootloader/bootloader.bin`, built from ESP-IDF with
`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`, and `espflash.toml` makes every flash
use it. It gives a `New` image one boot; if the chip resets before the image
marks itself valid, it marks it aborted and boots the last valid slot.

- **The safety net is not part of what is updated.** Rollback decided by the
  new image's own early code (the alternative) fails exactly when an update
  breaks that code, leaving a crash loop that only a USB cable ends — inside
  a closed feeder.
- **So the firmware's part is small:** mark itself valid on CONNACK, and on
  the broker timeout simply reset. A hang must reset too, which is the
  watchdog's job; a bootloader only acts on a reset.
- **Built reproducibly** by `dev/bootloader.sh` in `espressif/idf:v6.0` with
  `CONFIG_APP_REPRODUCIBLE_BUILD`, so the committed binary can be checked by
  rebuilding it. It is pinned rather than following espflash's releases.
- **Written only over USB.** OTA never touches it, so it must be right before a
  unit is closed up.

Rejected: rollback in the firmware with a boot counter (above), which stays
the fallback if this bootloader ever has to go.
