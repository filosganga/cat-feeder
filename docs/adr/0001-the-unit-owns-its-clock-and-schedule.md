# The unit owns its clock and its schedule

A feeder keeps its meals in its own flash and its time in a DS3231 on a coin
cell, so it keeps feeding with no broker and no Home Assistant. The question
that forced it: what does someone who did not build this have to install before
the feeder works? With the schedule held by Home Assistant the answer was a YAML
package, and without it a unit sat `online` and never fed — correct behaviour,
indistinguishable from a fault. Every comparable product answers it the same
way: the device owns its configuration and the app is a control surface.

- **A new unit starts blank.** Being given meals is an explicit act
  (`feeder/<id>/schedule`, the `Meal n` entities, the admin page), never
  inherited from a retained topic, so a board on the bench pointed at the house
  broker does not start turning. Blank fails toward not feeding.
- **Blank must be visible**: `"meals":0` in the state, `NO MEALS SET` on the
  panel, a console line at boot. Otherwise it looks exactly like a healthy
  feeder.
- **The clock is a DS3231, chosen for its oscillator-stop flag** rather than
  its accuracy. `OSF` lets the firmware read "never set, or the cell went flat"
  as a fact instead of trusting a plausible-looking date. A PCF8563 has no
  equivalent.
- **No NTP.** The clock is corrected by Home Assistant's live `feeder/time`, by
  hand on the knob or admin page, and kept across power cuts by the RTC.

## Considered options

- *Schedule in a shared retained topic, Home Assistant owning it* — the
  original design. A wiped broker blanked every unit, and a new unit fed meals
  nobody chose for it.
- *SNTP plus on-device timezone rules* — needs a network to own the clock at
  all, which only moves the dependency. See ADR-0016 for how summer time is
  handled instead.

## Consequences

Home Assistant can no longer see the schedule as one fact; it sees each unit's
entities. Making several units match is a copy (ADR-0015).
