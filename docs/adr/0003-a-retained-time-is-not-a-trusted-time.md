# A retained time is not a trusted time

`feeder/time` is retained, and if Home Assistant stops while Mosquitto keeps
running, the retained message can be any age; nothing in the payload says so.
Feeding from it would work through the day's slots at the wrong times. So the
clock separates *having* a time from *trusting* one:

| Source | Starts the clock | Arms the schedule |
|---|---|---|
| retained `feeder/time` | yes, if not yet trusted | no |
| live `feeder/time` (retain flag cleared by the broker) | yes | yes |
| DS3231 with `OSF` clear, at boot | yes | yes |
| set by hand (knob, admin page) | yes | yes |
| DS3231 with `OSF` set | no | no |

Once trusted, retained times are ignored outright (every reconnect replays one)
and an RTC read never overrides a trusted clock; the RTC is rewritten from live
times, not the other way round. Trust never lapses.

The live/retained distinction relies on the subscription leaving
`retain_as_published` off. To avoid waiting up to a minute for the next
periodic publish, a unit publishes `feeder/time/request` once per connection,
after subscribing, and Home Assistant answers immediately.

## Consequences

A unit whose RTC cannot be trusted, rebooted while Home Assistant is down, does
not feed until a live time arrives — three red flashes on the LED. That is
deliberate.
