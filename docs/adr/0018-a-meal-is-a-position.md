# A meal is a position in the schedule

Home Assistant shows meal *n* as *Meal n time* and *Meal n portions*, read from
the retained `feeder/<id>/schedule/state` echo, so flash stays the only copy.
Edits are non-retained commands to `feeder/<id>/meal/<n>/time|portions`.

- **Zero portions switches a meal off and keeps its place** — removing it would
  renumber every later meal under the user's feet. Off slots never feed and are
  not counted.
- **A time past the end adds the meal switched off**, padding gaps with off
  slots. A time alone never feeds.
- **Portions past the end are refused**: there is no time to feed them at.
- A refused edit republishes the unchanged echo, so the entity springs back.
- Retained edits are refused when replayed at subscribe time. (A retained
  publish *is* acted on by a unit already subscribed, so never publish these
  retained.)

The schedule channel is a queue, not a `Signal`: a time and a portion count
sent a moment apart are two edits.
