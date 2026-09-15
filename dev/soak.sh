#!/usr/bin/env bash
# Capture the serial console for hours, to a file you read the next day.
#
#   ./dev/soak.sh                # 12 hours
#   ./dev/soak.sh 3              # 3 hours
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

cd "$(dirname "$0")/.."

HOURS="${1:-12}"
SECONDS_TO_CAPTURE=$(awk -v h="$HOURS" 'BEGIN { printf "%d", h * 3600 }')

PORT="${ESPFLASH_PORT:-}"
if [ -z "$PORT" ]; then
  PORT=$(awk -F'"' '/^ESPFLASH_PORT=/ {print $2}' .cargo/config.toml)
fi
if [ -z "$PORT" ]; then
  echo "No serial port. Set ESPFLASH_PORT, or see:" >&2
  echo "  espflash list-ports --list-all-ports" >&2
  exit 2
fi

mkdir -p soak
LOG="soak/$(date +%Y-%m-%d_%H%M).log"

cat <<EOF
soaking ${HOURS}h on ${PORT}
log: ${LOG}

The board resets when the monitor attaches, so this starts from boot.
Leave it. Read it in the morning with:

  ./dev/soak-report.sh ${LOG}

EOF

# Unbuffered through sed so the file is readable while it is still being
# written, rather than only once the capture ends.
timeout "$SECONDS_TO_CAPTURE" espflash monitor \
  --non-interactive --port "$PORT" 2>&1 \
  | sed -u 's/\x1b\[[0-9;]*m//g' \
  | tee "$LOG"
