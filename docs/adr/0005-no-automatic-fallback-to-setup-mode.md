# No automatic fall back to setup mode; resets forget only the network

The boot path has one question: is there a usable record? If not, setup mode.
A unit that fails to connect does **not** fall back to setup: a router
rebooting for five minutes must not drop a working feeder into setup and stop
it feeding. Setup mode has no timeout either; it waits for someone.

Getting into setup deliberately is a physical gesture — the knob's click held
through power-on, or BOOT held five seconds while running. Both **forget the
network and keep the calibration, meals and timezone** (`Record::without_network`):
erasing everything lost a bench-measured calibration the first time it was
used. The menu's *Factory reset* is the one path that erases everything, behind
a `Keep`/`Erase` choice.

The knob gesture is a *boot* gesture, not a long hold while running, because the
same click feeds: separating feeding from erasing by hold duration alone means
a beat too long wipes a working feeder.
