#!/usr/bin/env bash
# Capture the serial console without reflashing.
#
#   ./dev/capture.sh                    # 45 s of everything
#   ./dev/capture.sh 70 'feed:|switch:' # 70 s, only matching lines
#
# The board resets when the monitor attaches, so a capture always starts from
# boot. Give it a couple of seconds before pressing anything.
#
# Same output shape as dev/flash.sh: each line annotated with the gap since the
# previous one.

set -euo pipefail

cd "$(dirname "$0")/.."

SECONDS_TO_CAPTURE="${1:-45}"
FILTER="${2:-}"

PORT="${ESPFLASH_PORT:-}"
if [ -z "$PORT" ]; then
  PORT=$(awk -F'"' '/^ESPFLASH_PORT=/ {print $2}' .cargo/config.toml)
fi
if [ -z "$PORT" ]; then
  echo "No serial port. Set ESPFLASH_PORT, or see:" >&2
  echo "  espflash list-ports --list-all-ports" >&2
  exit 2
fi

LOG=$(mktemp "${TMPDIR:-/tmp}/cat-feeder-capture.XXXXXX")

echo "capturing ${SECONDS_TO_CAPTURE}s on ${PORT}"
echo

# Deliberately not --no-reset: it halts the application with a flash stub and
# the capture then contains only bootloader output.
timeout "$SECONDS_TO_CAPTURE" espflash monitor \
  --non-interactive --port "$PORT" >"$LOG" 2>&1 || true

exec "$(dirname "$0")/_render.sh" "$LOG" "$FILTER"
