#!/usr/bin/env bash
# Tails everything the firmware and Home Assistant say to each other.
#
#   ./dev/watch.sh                  # feeder/# and homeassistant/#
#   ./dev/watch.sh 'feeder/#'       # just one filter
#
# Against another broker — the Pi, during roadmap step 11 — set MQTT_HOST, and
# MQTT_PASS with it if that broker's feeder user has a different password:
#
#   MQTT_HOST=192.168.68.50 MQTT_PASS=... ./dev/watch.sh 'feeder/time'
#
# Ctrl+C to stop.

set -euo pipefail

cd "$(dirname "$0")/.."

MQTT_HOST="${MQTT_HOST:-}"
MQTT_USER="${MQTT_USER:-feeder}"
MQTT_PASS="${MQTT_PASS:-feeder-dev}"

if [ $# -gt 0 ]; then
  topics=()
  for t in "$@"; do topics+=(-t "$t"); done
else
  topics=(-t 'feeder/#' -t 'homeassistant/#')
fi

# The local dev stack is watched from inside its own container, so nothing has
# to be installed on the Mac and the password file is the one already mounted.
if [ -z "$MQTT_HOST" ]; then
  exec docker compose exec mosquitto \
    mosquitto_sub -h localhost -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
fi

# Another broker. Prefer a mosquitto_sub on the PATH, so watching the Pi does
# not require the Mac's stack to be running — which is the whole point of having
# moved to the Pi. Fall back to the container, which can reach the LAN as well.
if command -v mosquitto_sub >/dev/null 2>&1; then
  exec mosquitto_sub -h "$MQTT_HOST" -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
fi

echo "watch: no local mosquitto_sub; going through the dev stack's container." >&2
echo "       brew install mosquitto to avoid needing it running." >&2
exec docker compose exec mosquitto \
  mosquitto_sub -h "$MQTT_HOST" -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
