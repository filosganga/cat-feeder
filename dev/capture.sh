#!/usr/bin/env bash
# Capture the serial console without reflashing.
#
#   ./dev/capture.sh                             # 45 s of everything
#   ./dev/capture.sh --seconds 70 --filter 'feed:|switch:'
#   ./dev/capture.sh --port /dev/cu.usbmodemXXXX
#
# The first two are also positional, as they always were: `./dev/capture.sh 70
# 'feed:'` still works. --port wins over ESPFLASH_PORT; see dev/_common.sh.
#
# The board resets when the monitor attaches, so a capture always starts from
# boot. Give it a couple of seconds before pressing anything.
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

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --seconds) need_value "$1" "${2:-}"; SECONDS_TO_CAPTURE="$2"; shift 2 ;;
    --seconds=*) SECONDS_TO_CAPTURE="${1#*=}"; shift ;;
    --filter) need_value "$1" "${2:-}"; FILTER="$2"; shift 2 ;;
    --filter=*) FILTER="${1#*=}"; shift ;;
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

echo "capturing ${SECONDS_TO_CAPTURE}s on ${PORT}"
echo

# Deliberately not --no-reset: it halts the application with a flash stub and
# the capture then contains only bootloader output.
timeout "$SECONDS_TO_CAPTURE" espflash monitor \
  --non-interactive --port "$PORT" >"$LOG" 2>&1 || true

exec "$DEV_DIR/_render.sh" "$LOG" "$FILTER"
