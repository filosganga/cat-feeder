# The broker is configured as an IPv4 address

The firmware has no DNS resolver; `mqtt.rs` parses `mqtt_host` as an
`Ipv4Addr`. Every form rejects a hostname with a clear message, because a
stored hostname would survive the reboot and leave the unit retrying a
connection it can never make. `HOST_LEN` is 64 to leave room for DNS later.

## Consequences

The broker's machine needs a DHCP reservation. If its address moves, every
unit must be re-provisioned (`provision.sh --host`, or the admin page).
