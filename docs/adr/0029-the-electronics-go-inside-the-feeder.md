# The electronics go inside the feeder

Supersedes [ADR-0012](0012-electronics-live-in-their-own-case.md).

The perfboard goes inside the feeder's own case, where the original board was,
not into a separate printed enclosure. ADR-0012 chose a case of its own for two
reasons: so one design fitted every brand of feeder, and so a panel and a knob
could sit out of a cat's reach. With no panel and no knob (ADR-0027), nothing
on the board has to be outside the feeder, and it fits where the original
board was. There is nothing to design, print or mount, and no cable leaves the
feeder.

What follows. **Whether the LED and BOOT are brought to the outside of the
feeder is not decided**; for now both are on the board only.

- **The LED is seen only if the board sits behind a window or a hole**, such as
  the old display window, or a light pipe reaches it. Where it cannot be seen,
  the faults that cut a unit off from Home Assistant (no Wi-Fi, no broker, no
  time) are the ones nothing shows: Home Assistant only sees the unit go
  unavailable. The LED was kept for exactly those (ADR-0011).
- **BOOT is reached only by opening the feeder**, unless a hole or a button in
  parallel (the Zero has BOOT on a back pad) brings it outside. It is needed
  only to forget the network of a unit that cannot reach it; until then such a
  unit is opened up, or re-provisioned over USB (`dev/provision.sh`). A unit
  that is online has the network form and the factory reset on its admin page.
- The USB-C socket and the RTC's coin cell are reached by opening the feeder.
- The label goes on the outside of the feeder.
