# Pause is broker state, and Home Assistant is its authority

`feeder/<id>/paused` is retained per unit — the one setting still held by the
broker rather than in flash — so a unit rebooting while paused comes back
paused. There is no `feeder/all/paused`: two retained topics setting one flag
would race on reconnect.

The Home Assistant package republishes every unit's `paused` from a helper on
every start and every change. A pause set from the knob is published (retained)
so the replay does not undo it, but it is a **temporary override** until Home
Assistant next republishes.

The package **publishes to the topic** rather than calling `switch.turn_on`:
Home Assistant drops unavailable entities from service calls, so pausing an
unplugged unit would silently do nothing. It finds the units from the device
registry by the `model` in `discovery.rs`, so that string is a contract between
the firmware and the package.

Pause stops the schedule, not the feeder: manual feeds still work.
