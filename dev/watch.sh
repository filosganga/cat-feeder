#!/usr/bin/env bash
# Tails everything the firmware and Home Assistant say to each other.
#
#   ./dev/watch.sh                  # feeder/# and homeassistant/#
#   ./dev/watch.sh 'feeder/#'       # just one filter
#
# Against another broker — the Pi, during roadmap step 11 — name it, and give it
# a password too if that broker's feeder user has a different one:
#
#   ./dev/watch.sh --host 192.168.68.126 'feeder/time'
#   ./dev/watch.sh --host 192.168.68.126 --user feeder --password ... 'feeder/#'
#
# MQTT_HOST, MQTT_USER and MQTT_PASS still work; the flags win over them. See
# dev/_common.sh for why both exist.
#
# Ctrl+C to stop.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

HOST_ARG=""
USER_ARG=""
PASS_ARG=""
topics=()

while [ $# -gt 0 ]; do
  case "$1" in
    --host) need_value "$1" "${2:-}"; HOST_ARG="$2"; shift 2 ;;
    --host=*) HOST_ARG="${1#*=}"; shift ;;
    --user) need_value "$1" "${2:-}"; USER_ARG="$2"; shift 2 ;;
    --user=*) USER_ARG="${1#*=}"; shift ;;
    --password|--pass) need_value "$1" "${2:-}"; PASS_ARG="$2"; shift 2 ;;
    --password=*|--pass=*) PASS_ARG="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; while [ $# -gt 0 ]; do topics+=(-t "$1"); shift; done ;;
    -*) die "watch: unknown option '$1'. Try --help." ;;
    *) topics+=(-t "$1"); shift ;;
  esac
done

MQTT_HOST="${HOST_ARG:-${MQTT_HOST:-}}"
MQTT_USER="${USER_ARG:-${MQTT_USER:-feeder}}"
MQTT_PASS="${PASS_ARG:-${MQTT_PASS:-feeder-dev}}"

if [ ${#topics[@]} -eq 0 ]; then
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
