# Wi-Fi and broker credentials come from flash and nowhere else

Nothing secret is compiled into the binary. A record in the `nvs` partition
(magic + CRC-32, so erased flash and an interrupted write read as
*unconfigured*) holds the network, the broker and the unit's calibration. It is
written either over USB by `dev/provision.sh` (which builds it on the host with
the same `provisioning.rs` code) or by the unit's own setup network and form.
`espflash` never rewrites `nvs`, so configuration survives every reflash.

One binary therefore flashes every unit, and a release binary never carries a
Wi-Fi password for a house it may never be installed in.

The one build-time value is `ap_secret`, a salt: each unit's setup-network and
admin password is `base32(sha256("<ap_secret>:<id>"))`. Without the salt the
password would be derivable from the MAC in the beacon, and WPA2-PSK protects
nothing from someone who knows the passphrase — including the session where the
home Wi-Fi password is typed into the form. `dev/ap-password.sh` and the
firmware must agree byte for byte, which is why the derivation is plain
SHA-256 and pinned by a test against an independent implementation.

## Considered options

*cfg.toml defaults seeded into flash at first boot* — pleasant for development,
but makes compiled-in credentials permanent, and it refilled flash on every
empty boot, which made setup mode unreachable.
