---
name: pcb-review
description: Reviews the perfboard drawing, pcb.diy (DIY Layout Creator), before anything is soldered or powered — runs dev/pcb-check.sh, reads its netlist, pinout, unconnected-pin and neighbouring-net reports, and says what the check cannot see. Use whenever pcb.diy changes, when asked whether a layout is right or safe to solder, when a new module or header is added to the board, when a soldered unit does nothing at power-on, or when writing the beep-test table in CLAUDE.md's *The perfboard*.
---

# Reviewing the perfboard

`pcb.diy` is the only layout; CLAUDE.md's *The perfboard* describes it. A
drawing can look right on screen and still be wrong in ways that cost a part,
so it is checked by a script, not by eye.

```sh
./dev/pcb-check.sh            # exit 1 on a real fault
./dev/pcb-check.sh --all      # ...with the signal-to-signal neighbours too
```

**Run it after every edit to `pcb.diy`, and always before soldering.** Read the
file from disk, not a screenshot: DIY Layout Creator only writes on save, so ask
whether the drawing is saved before trusting a result, and rerun after it is.

## What each section means

| Section | Fails on | What it caught here |
|---|---|---|
| Pinouts | a header's names disagree with `dev/pinouts.toml` | the DRV8833's `Vcc`/`GND` drawn swapped: a reversed supply on first power |
| Power | ground joined to a supply, 5 V to 3V3, an electrolytic's `-` on a supply | |
| Unconnected pins | a pin connected to nothing that is not listed `unused` | the switch line one jumper short of GPIO2 |
| Labels | a printed label on a pin of another name | |
| Neighbours | never; it ranks | 5 V beside ground, 5 V beside GPIO2, 3V3 beside `In1` |

Both catches in that table are real: they were the first draft's two faults,
and the draft (`git show f90156f:pcb-v2.diy`) still fails on exactly those two.
It is a good regression test for a change to the script.

**Neighbours are for reading, not passing.** Every perfboard has nets a hole
apart. What matters is the rank:

- `short` — a supply beside ground. A blob there shorts the rail.
- `high` — 5 V beside anything else. Beside a GPIO it destroys the pin (the C6
  is not 5 V-tolerant); between the USB and battery nets it charges the cells.
- `medium` — 3V3 beside a signal. Beside `In1` it runs the motor on its own.

For each `short` and `high` pair, either move one run, insulate it, or put a
guard in the way (R1 is one), and make sure the beep-test table in CLAUDE.md
has a line that would catch the bridge. A hole pair listed here is where a
beep-test probe goes.

## What it cannot see

Say these out loud in a review; a green run does not cover them.

- **A part's real pinout, unless it is in `dev/pinouts.toml`.** The netlist is
  only as good as the pin names in the drawing. A header with no entry prints
  `??` and is unchecked. When a module is added, copy its row **from the
  silkscreen on the part**, never from the drawing — the drawing is the thing
  being checked.
- **Which way round a part sits.** A row that matches backwards passes, since a
  module may be fitted either way; the check only insists that every row of one
  module agrees. That a reversed Zero also swaps its two rows is not checked.
- **The electrolytic's polarity on the real board** — only that the drawing's
  `+` lead is on a supply. DIYLC marks the first lead `+` unless the part is
  drawn inverted.
- **Anything DIYLC does not draw**: wire gauge, current, a part's value (R1 is a
  1 kΩ only because the drawing says so), mechanical fit, a header's keying.
- **Parts the script does not know.** They are listed under *Not checked*;
  extend `load()` in `dev/pcb-check.py` rather than ignoring the line.
- **The module's own header order on the bench**: socketed modules can still be
  plugged in a row off. That is the beep test's job, with modules in.

## When the board changes

1. Edit and save in DIY Layout Creator.
2. `./dev/pcb-check.sh` until it passes, then read the neighbours.
3. A new or moved pin: update `dev/pinouts.toml` — `pins` from the part,
   `unused` for what this layout leaves unconnected on purpose.
4. Update *The perfboard* in CLAUDE.md: the header table, the beep-test table
   (hole names from this board's `A … X`, rows from the bottom), and the list
   of places a blob does damage — the `short` and `high` pairs.
5. Only then solder, beep with modules in, and power from USB alone.
