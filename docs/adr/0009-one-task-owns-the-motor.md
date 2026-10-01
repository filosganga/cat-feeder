# One task owns the motor; decisions live in a pure state machine

Only the feeder task touches the motor and the hub switch. Producers (MQTT,
schedule, knob, admin page) `try_send` portion counts into one channel and log
a discard if it is full; nothing shares mutable state. The decisions are in
`feeder::Feeder`, which takes time as milliseconds per call and so is fully
host-tested; the task asks it what to do and reports back.

- **The feed channel is a branch of the `select`, not drained before it.**
  Draining with `try_receive` at the top of the loop blocks for a whole jam
  budget, so a request arriving mid-turn is seen only after the motor has
  braked — three `feed 1` within 170 ms produced one portion on hardware.
- The motor does not stop between portions; requests arriving mid-turn extend
  the turn.
- **A jam discards everything pending** — resuming a queue into a jammed
  mechanism is worse than dropping a meal. **Nothing gates on the jam flag**:
  the next request just tries again and its first click clears it, so a jam
  never needs a power cycle. There is deliberately no reverse, which could jam
  the mechanism against its own geometry; a retry recovers a transient stall,
  not a real blockage.
