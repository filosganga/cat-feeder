#!/usr/bin/env bash
# Capture the serial console for hours, to a file you read the next day.
#
#   ./dev/soak.sh                # 12 hours
#   ./dev/soak.sh --hours 3      # 3 hours
#   ./dev/soak.sh --port /dev/cu.usbmodemXXXX
#
# The hours are also positional, as they always were: `./dev/soak.sh 3` still
# works. --port wins over ESPFLASH_PORT; see dev/_common.sh.
#
# Everything else in dev/ answers "did this change work", in a couple of
# minutes. This answers the questions that only appear over a night:
#
#   * Does a scheduled slot fire unattended, with nobody stepping the clock?
#   * Does the heap survive thousands of MQTT packets? rust-mqtt's AllocBuffer
#     allocates per received packet, and the unit takes a time message every
#     minute against a 36 KB heap.
#   * Does the connection come back after Wi-Fi or the broker blinks?
#   * Does the local clock drift between alignments?
#
# Does NOT reflash: flash first, then start this. It leaves the board running
# whatever is already on it.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

HOURS=""
PORT_ARG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --hours) need_value "$1" "${2:-}"; HOURS="$2"; shift 2 ;;
    --hours=*) HOURS="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "soak: unknown option '$1'. Try --help." ;;
    *)
      if [ -z "$HOURS" ]; then HOURS="$1"
      else die "soak: unexpected argument '$1'. Try --help."
      fi
      shift
      ;;
  esac
done

HOURS="${HOURS:-12}"
SECONDS_TO_CAPTURE=$(awk -v h="$HOURS" 'BEGIN { printf "%d", h * 3600 }')

PORT="$(require_port "$PORT_ARG")"

mkdir -p soak
LOG="soak/$(date +%Y-%m-%d_%H%M).log"

cat <<EOS
soaking ${HOURS}h on ${PORT}
log: ${LOG}

The board resets when the monitor attaches, so this starts from boot.
Leave it. Read it in the morning with:

  ./dev/soak-report.sh ${LOG}

EOS

# Unbuffered through sed so the file is readable while it is still being
# written, rather than only once the capture ends.
timeout "$SECONDS_TO_CAPTURE" espflash monitor \
  --non-interactive --port "$PORT" 2>&1 \
  | sed -u 's/\x1b\[[0-9;]*m//g' \
  | tee "$LOG"
