# The knob is the outside control, and a hold opens the menu

The adversary is cats: a control on a cat feeder that dispenses food is a
control cats will learn to use. So the vocabulary is **hold to unlock/lock, tap
to act, turn to move**, and turning never dispenses. The only route to food at
the unit is a hold, then a tap on `Feed`.

A rotary encoder (EC11) was chosen as the single outside control — its shaft
switch is the button — because a knob and a panel are the only configuration
route that needs no network and no second device.

## Considered options

*A recessed feed button plus a separately placed knob* was the cleaner design:
the knob would be physically unable to dispense, and the recess defeats a paw
outright. It was declined for one control and simpler wiring. The cost is that
a knob cannot be recessed, so **placement** — the case mounted high or behind
the feeder — is the mechanical defence (ADR-0012).

## Consequences

- A lock must come from a different press than the unlock (a four-second hold
  otherwise unlocks and relocks).
- Unlocking always lands on `Feed`; the menu locks itself after 10 s idle.
- Future settings editors confirm with a tap on an explicit item, never a hold.
- A count held between taps would be a default portion size, which ADR-0007
  rejects; changing that is a reversal to argue for, not a fix.
