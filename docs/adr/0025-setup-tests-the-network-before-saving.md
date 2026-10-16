# Setup tests the network before it saves, and stays a SoftAP

Setup runs the radio as access point *and* station. The form's settings are
tried on the station half while the phone stays on the setup network: join,
DHCP lease, broker login. Only a Wi-Fi that joined is saved; a broker that
refuses is reported, with *Save anyway*, because a unit may be set up before
its broker exists and the running unit retries the broker forever. The
reason is ADR-0005: once saved, a unit never returns to setup by itself, so a
typo used to mean a unit retrying forever until someone held BOOT. The cost is
that the access point follows the router's channel when the station joins, and
the phone drops off the setup network on every trial: starting one re-applies
the access point's config, and esp-radio offers no way to set the station half
alone. Searching every channel made it worse, long enough for a phone to give
up. So a trial joins the node the boot scan heard best, on a channel the
access point has already moved to; the page warns of the drop before it
happens and polls `/status`; and `/` shows the last outcome, which is where a
phone that rejoins — by itself, within seconds, on the test — lands.

The SoftAP stays the one way in. Improv only carries an SSID and password, not
the broker this unit also needs; over BLE it needs esp-radio's `ble` and `coex`
features and a GATT host stack, over serial it duplicates `dev/provision.sh`,
and a generic ESP Web Tools page cannot work when every build has its own
`ap_secret`. Espressif's BLE provisioning needs their app; Web Bluetooth has no
Safari. WPS, Wi-Fi Easy Connect (DPP) and SmartConfig have no esp-radio API.
