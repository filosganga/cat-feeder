# A missed meal is preferable to a double one

When the firmware cannot be sure, it does not feed. Overfeeding is invisible
and cumulative; a missed meal is noticed. Three guards in `schedule.rs`, each
covering a failure the others cannot see:

- a **consumed marker** in RAM, `(day, minute-of-day)` of the last slot
  resolved — keyed on time of day, not slot index, because a schedule can be
  republished with a slot inserted and an index would then point at a
  different meal;
- a **baseline pass**: the first look at the clock after boot, *once a
  schedule has arrived*, only records where the day is and never feeds — so a
  reboot at 19:01 does not serve the 19:00 meal;
- a **lateness limit** of two minutes, so a clock jumping forward is never
  mistaken for a slot falling due.

Only a *later date* re-arms the day's slots; a clock moved back across midnight
keeps the marker, so a date set wrong and then corrected cannot serve a meal
twice. Slots that pass while paused are consumed, not fed; resuming never
catches up.

The same principle gives *power-cycled with no trustworthy time → wait, never
guess* (ADR-0003) and *a jam discards whatever is pending*.
