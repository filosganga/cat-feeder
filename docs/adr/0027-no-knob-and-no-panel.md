# No knob and no panel: every unit is headless

Supersedes [ADR-0010](0010-the-knob-is-the-outside-control.md) and
[ADR-0021](0021-headless-is-a-build-flag.md); amends
[ADR-0005](0005-no-automatic-fallback-to-setup-mode.md) and
[ADR-0011](0011-the-led-is-dark-when-healthy.md).

The rotary encoder, its menu and the OLED panel are removed, and there is no
`headless` feature: there is only one build, and GPIO3–GPIO5 are not read. The
knob was chosen because it was the one way to configure a unit with no network
and no second device (ADR-0010), and that turned out not to be useful: every
unit is set up from a phone on its setup network (ADR-0025) and run from Home
Assistant and the admin page, which already does everything the menu did:
feeding, pausing, the clock and timezone, the calibration and a calibration run.
The perfboard carries no knob or panel. Keeping the code would have meant a
second interface to keep in step with every change, built and linted in CI and
run on no unit: about 5,000 lines, 120 host tests and two crates.

**The code is kept by the tag `last-with-knob-and-panel`**, commit
`6da8039bde05a0a9bf02e553a39b32ab265e99a7`: `src/menu.rs`, `display.rs`,
`encoder.rs`, `button.rs` and `oled.rs`, and in `src/bin/main.rs` the
`ui_task`, `encoder_task` and `display_task` that wired them, e.g.
`git show last-with-knob-and-panel:src/menu.rs`. Bringing them back means a new
ADR and a port, not a checkout: the bus, the admin page and the LED will have
moved on.

What follows:

- **The factory reset moves to the admin page and the setup page**
  (`POST /erase` on both), behind a ticked box and a browser `confirm`. The
  setup page's copy is the route for a unit with no working network: BOOT held
  five seconds reaches setup mode, and the setup network's password guards the
  erase. The BOOT hold still forgets only the network, as ADR-0005 has it. A
  longer hold that erased everything was declined, because it would separate a
  mild reset from a destructive one by hold length alone, which is the
  accident ADR-0005 records.
- The knob's power-on gesture goes with it; BOOT is the only physical control.
- The LED loses the menu's cyan *armed* state. The order is now reset hold,
  jam, feeding, setup, the three network faults, paused.
- The setup password is on the console and the sticker only, so the sticker is
  required, as ADR-0021 already said for headless units.
- The DS3231 has the I²C bus to itself, so the shared-bus handle and
  `embassy-embedded-hal` go.
- A `manual` event's `source` is always `web`; the attribute stays so the
  payload keeps its shape.
