#!/usr/bin/env bash
# Tails everything the firmware and Home Assistant say to each other.
#
#   ./dev/watch.sh                  # feeder/# and homeassistant/#
#   ./dev/watch.sh 'feeder/#'       # just one filter
#
# Ctrl+C to stop.

set -euo pipefail

cd "$(dirname "$0")/.."

MQTT_USER="${MQTT_USER:-feeder}"
MQTT_PASS="${MQTT_PASS:-feeder-dev}"

if [ $# -gt 0 ]; then
  topics=()
  for t in "$@"; do topics+=(-t "$t"); done
else
  topics=(-t 'feeder/#' -t 'homeassistant/#')
fi

exec docker compose exec mosquitto \
  mosquitto_sub -h localhost -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
