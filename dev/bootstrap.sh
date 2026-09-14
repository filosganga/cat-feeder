#!/usr/bin/env bash
# Creates the Mosquitto password file for the local dev broker.
# Run once, before the first `docker compose up`.
#
#   ./dev/bootstrap.sh                  # user "feeder", password "feeder-dev"
#   ./dev/bootstrap.sh myuser mypass
#
# The password file is git-ignored. Whatever you set here also goes into the
# firmware's cfg.toml.

set -euo pipefail

cd "$(dirname "$0")/.."

MQTT_USER="${1:-feeder}"
MQTT_PASS="${2:-feeder-dev}"

docker run --rm \
  -v "$PWD/dev/mosquitto:/config" \
  eclipse-mosquitto:2 \
  mosquitto_passwd -c -b /config/passwd "$MQTT_USER" "$MQTT_PASS"

# Mosquitto 2 refuses to start if the password file is world-readable.
chmod 0600 dev/mosquitto/passwd

echo
echo "Broker credentials written to dev/mosquitto/passwd"
echo "  user:     $MQTT_USER"
echo "  password: $MQTT_PASS"
echo
echo "Reachable at:"
echo "  localhost:1883                     from this Mac"
echo "  mosquitto:1883                     from the Home Assistant container"
echo "  $(ipconfig getifaddr en0 2>/dev/null || echo '<mac lan ip>'):1883   from the ESP32 over Wi-Fi"
echo
echo "Next: docker compose up -d"
