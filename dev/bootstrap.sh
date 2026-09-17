#!/usr/bin/env bash
# Creates the Mosquitto password file for the local dev broker.
# Run once, before the first `docker compose up`.
#
#   ./dev/bootstrap.sh                  # user "feeder", password "feeder-dev"
#   ./dev/bootstrap.sh --user myuser --password mypass
#
# The two are also positional, as they always were: `./dev/bootstrap.sh myuser
# mypass` still works.
#
# The password file is git-ignored. Whatever you set here also goes into the
# firmware's cfg.toml.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

MQTT_USER=""
MQTT_PASS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --user) need_value "$1" "${2:-}"; MQTT_USER="$2"; shift 2 ;;
    --user=*) MQTT_USER="${1#*=}"; shift ;;
    --password|--pass) need_value "$1" "${2:-}"; MQTT_PASS="$2"; shift 2 ;;
    --password=*|--pass=*) MQTT_PASS="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "bootstrap: unknown option '$1'. Try --help." ;;
    *)
      if [ -z "$MQTT_USER" ]; then MQTT_USER="$1"
      elif [ -z "$MQTT_PASS" ]; then MQTT_PASS="$1"
      else die "bootstrap: unexpected argument '$1'. Try --help."
      fi
      shift
      ;;
  esac
done

MQTT_USER="${MQTT_USER:-feeder}"
MQTT_PASS="${MQTT_PASS:-feeder-dev}"

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
