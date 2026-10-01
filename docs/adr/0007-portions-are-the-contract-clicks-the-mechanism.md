# Portions are the contract; clicks are the mechanism

Everything from outside speaks portions — the Home Assistant button, the feed
topics, every schedule slot, the knob. `portions::clicks_for` is the single
place a request becomes clicks, using a per-unit `portion_scale_pct` (100 =
unchanged), because one click need not dispense the same amount on different
mechanisms and nothing else in the system could see the difference.

- A request for one or more portions **never becomes zero clicks**, at any
  scale. `feed 0` stays a no-op.
- Rounding is to nearest, **per request, with no carried remainder**, so the
  same slot always gives the same number of clicks.
- `MAX_CLICKS` (16) caps clicks owed at once — what empties a hopper is clicks.
- `last_fed` and events report **portions as requested**, so Home Assistant's
  history matches its own automations.
- There is no default portion size: every feed path states its count, and
  manual feeds accumulate (three taps or presses = three portions, absorbed
  into the running turn).
