#!/usr/bin/env bash
# Capture the serial console without reflashing.
#
#   ./dev/capture.sh                             # 45 s of what the unit says now
#   ./dev/capture.sh --reset                     # ...from a fresh boot instead
#   ./dev/capture.sh --seconds 70 --filter 'feed:|switch:'
#   ./dev/capture.sh --port /dev/cu.usbmodemXXXX
#
# The first two are also positional, as they always were: `./dev/capture.sh 70
# 'feed:'` still works. --port wins over ESPFLASH_PORT; see dev/_common.sh.
#
# **By default it only listens**: the unit keeps running as it was, so a quiet
# unit may print nothing at all, and that is not an error. espflash's monitor
# cannot do this — it resets the chip when it attaches, even with its no-reset
# options — so the port is read by dev/_tap.py instead.
#
# That matters most after an OTA update: a reset before the new image has
# confirmed itself rolls it back (ADR-0024). When the unit resets on its own —
# an OTA restart, the watchdog — the Zero's USB port vanishes and returns, and
# the capture waits for it and carries on, so one listening capture shows an
# update from upload to "image confirmed".
#
# --reset is the old behaviour: espflash's monitor, which resets the chip, so
# the capture starts from boot. Use it to see a boot sequence, and give it a
# couple of seconds before pressing anything.
#
# Listening without a reset is seen working on the Zero's own USB port. On the
# dev kit's CH343 bridge, opening the port moves DTR and RTS, which drive its
# auto-reset circuit; if a plain capture resets a dev kit, that is why.
#
# Same output shape as dev/flash.sh: each line annotated with the gap since the
# previous one.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

SECONDS_TO_CAPTURE=""
FILTER=""
PORT_ARG=""
RESET=""

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --seconds) need_value "$1" "${2:-}"; SECONDS_TO_CAPTURE="$2"; shift 2 ;;
    --seconds=*) SECONDS_TO_CAPTURE="${1#*=}"; shift ;;
    --filter) need_value "$1" "${2:-}"; FILTER="$2"; shift 2 ;;
    --filter=*) FILTER="${1#*=}"; shift ;;
    --reset) RESET=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "capture: unknown option '$1'. Try --help." ;;
    *)
      if [ -z "$SECONDS_TO_CAPTURE" ]; then SECONDS_TO_CAPTURE="$1"
      elif [ -z "$FILTER" ]; then FILTER="$1"
      else die "capture: unexpected argument '$1'. Try --help."
      fi
      shift
      ;;
  esac
done

SECONDS_TO_CAPTURE="${SECONDS_TO_CAPTURE:-45}"
PORT="$(require_port "$PORT_ARG")"

LOG=$(mktemp "${TMPDIR:-/tmp}/cat-feeder-capture.XXXXXX")

if [ -n "$RESET" ]; then
  echo "resetting and capturing ${SECONDS_TO_CAPTURE}s on ${PORT}"
  echo

  # Deliberately not --no-reset: it halts the application with a flash stub
  # and the capture then contains only bootloader output.
  timeout "$SECONDS_TO_CAPTURE" espflash monitor \
    --non-interactive --port "$PORT" >"$LOG" 2>&1 || true

  exec "$DEV_DIR/_render.sh" "$LOG" "$FILTER"
fi

echo "listening ${SECONDS_TO_CAPTURE}s on ${PORT} (no reset)"
echo

python3 "$DEV_DIR/_tap.py" "$PORT" "$SECONDS_TO_CAPTURE" >"$LOG" 2>&1 || true

exec "$DEV_DIR/_render.sh" "$LOG" "$FILTER" listening
