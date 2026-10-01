# The unit keeps summer time from a POSIX rule its admin page derives

Home Assistant's live time carries its own offset and stays the authority
whenever it is there (offsets are kept, never converted away: `08:00` in a slot
means 08:00 on the kitchen wall). After ten minutes without one, the unit
follows its own zone.

The unit stores a **name** (`Europe/Rome`, for people) and a **POSIX TZ rule**
(for the clock) and never turns one into the other. The admin page derives the
rule in the browser from the tz data browsers keep current, so a change in law
needs the page reopened, never a firmware update. The DS3231 keeps the offset
its time is in (in alarm 2's unused registers), because local time alone is
ambiguous across a change.

## Considered options

*A zone table compiled into the firmware* — goes stale and only a reflash
refreshes it. Rules for zones whose law POSIX cannot express (e.g. Santiago,
Casablanca) are right for the current year only; a live time corrects them.
