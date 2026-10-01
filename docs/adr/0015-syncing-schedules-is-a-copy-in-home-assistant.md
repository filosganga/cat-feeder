# Making feeders share meals is a copy in Home Assistant, not a broadcast topic

`script.cat_feeder_copy_schedule` reads one unit's meals from its `Meal n`
entities and publishes them to each target's own `feeder/<id>/schedule`, not
retained. Targets are every feeder, or those picked by device, label, area or
floor. A copy from an offline unit is refused, because its entities are
`unavailable`.

## Considered options

*`feeder/all/schedule`, a non-retained broadcast* — removed. A schedule fires
from each unit's own clock, so copies arriving milliseconds apart lose nothing,
and one path for "all" and for subsets is simpler than two. (`feeder/all/feed`
stays: a feed is an instant.)

*Groups in the firmware* (`feeder/group/<name>/…`) are only worth building if
syncing must happen with no Home Assistant. If ever built: `feeder/all/*` must
still reach everyone, groups must apply to `feed` as well as `schedule`, and a
unit's group must be visible in its state, since a typo is otherwise silent.
