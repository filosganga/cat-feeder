# Feeding counts falling edges, and spacing is filtered in the feeder

`feed(n)` aligns (if the switch is open, run until the first falling edge,
uncounted), counts `n` falling edges, and brakes **on** the nth, so the hub
parks in the same place. The starting level never matters, because the hub can
be turned by hand and the first run after assembly starts anywhere.

`switch.rs` debounces at 30 ms and reports **every** edge. The minimum-spacing
rejection (ADR-0006) lives in `feeder.rs`, because it is only true while the
motor drives: a bench button pressed twice quickly gives real edges far closer
together, and a stream that swallowed them would lie.

**The align phase is exempt from spacing.** A run starting with the switch open
is at an unknown rotor position and may be a hair short of the next detent;
rejecting that genuine early edge spends another detent finding the next one,
and that quarter turn dispenses food nothing counts. While aligning, only the
jam timeout bounds the run.

The jam timeout is the **remaining** budget from the last counted click, so
repeated bounce can never postpone jam detection.
