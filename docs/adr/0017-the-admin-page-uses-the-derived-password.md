# The admin page authenticates with the unit's derived password

Every configured unit serves `http://<address>/` alongside MQTT. An always-on
listener on the house network is permanent attack surface, so:

- **Basic auth, any username, the derived password** (ADR-0004) — the same as
  the setup network and the sticker. There is no field to set one, so a unit is
  never unauthenticated, and recovery is the physical reset.
- **A POST whose `Origin` is not this unit is refused**, since a logged-in
  browser resends Basic credentials on a form another page submits. No
  `Origin` at all (curl) must bring the password itself.
- **Stored passwords are never rendered**; empty means keep.
- Plain HTTP on the LAN, like comparable devices — stated, not hidden.
- Saving the network restarts the unit; saving meals, clock or calibration does
  not.

Discovery publishes `configuration_url` with the address at connect time. mDNS
was rejected: `.local` is unreliable over mesh networks and browsers with
Secure DNS send it upstream.
