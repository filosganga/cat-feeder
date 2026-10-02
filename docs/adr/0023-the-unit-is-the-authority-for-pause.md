# The unit is the authority for pause

Supersedes [ADR-0014](0014-home-assistant-is-the-authority-for-pause.md).

The pause is kept in the unit's flash (`FDP1`, its own sector) like the
schedule, and changed only through `Bus::set_pause`: flash first, then in
force. The knob's menu, the admin page and a **live** `feeder/<id>/paused`
command all take that path, so a unit pauses and resumes with no Home
Assistant and no broker, and comes back from a reboot as it was left. The
state payload's `paused` is the unit's answer, and what Home Assistant's
switch shows.

`feeder/<id>/paused` is now an ordinary command: never retained, and a retained
one is refused, as a retained schedule is. Home Assistant's package asks only
when its helper changes, never at its own start. The cost: a unit that is
offline when the helper changes misses the pause and keeps feeding, and the
helper does not re-apply itself when the unit comes back. That is accepted —
a missed pause feeds the cats, and the switch shows what each unit holds.

Upgrading from ADR-0014's firmware starts with no `FDP1`, so the unit runs,
and the old retained flag is refused: a unit paused through the broker comes
back running and has to be paused again.

Pause stops the schedule, not the feeder: manual feeds still work. There is
still no `feeder/all/paused`.
