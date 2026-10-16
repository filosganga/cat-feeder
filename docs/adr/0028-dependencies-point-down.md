# Dependencies point down: core, state, controls

The firmware is four layers, and a module depends only on its own layer or
the ones below it:

1. **Core**: what a feeder is and decides (feeding, portions, calibration,
   the schedule and its clock, timezones, the RTC's registers, events).
2. **State**: what the unit holds (the flash records, the store, `wiring::Bus`),
   with one operation per change that every control calls, as
   `Bus::set_pause` is.
3. **Controls**: the admin page, setup mode, MQTT and Home Assistant
   discovery, the LED and the BOOT hold. Each one parses its input, calls an
   operation, and renders what came back.
4. `main.rs`, which wires them together.

Pure versus hardware is a separate axis. A host-tested module is not low for
being pure: `admin.rs` is pure and sits with the controls.

The knob is why this is written down (ADR-0027). The core was pure logic, but
the UI had leaked into the layers below it. `Calibration` and `Field` were
defined in `menu.rs` and imported by the core. The menu's page, cursor and last
press lived on the shared bus. The LED's priority had a slot for the menu, the
boot order bent around the panel's probe, and core comments described the
knob's readings. Removing that one control touched 51 files. Under this rule it
would have touched the control, `main.rs` and its docs.

The test for a change is the same: removing a control should touch only the
control, `main.rs` and its docs. A control's own state stays in its task. A
type that a control and the core both need is defined in the core, and the
core's comments name an input ("a time set by hand"), not the control that
sends it.

The cost is a little indirection: an operation on the state layer even where
only one control uses it today. That is accepted, because the next control,
such as a Home Assistant entity or a second page, then calls the same
operation rather than copying the first one's logic.
