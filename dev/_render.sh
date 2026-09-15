#!/usr/bin/env bash
# Renders a captured serial log. Used by flash.sh and capture.sh; not meant to
# be called directly.
#
#   _render.sh <logfile> [filter-regex]
#
# Strips colour codes, optionally filters, and annotates each line with the
# milliseconds elapsed since the previous shown line.

set -euo pipefail

LOG="$1"
FILTER="${2:-}"

PLAIN=$(sed 's/\x1b\[[0-9;]*m//g' "$LOG")

APP_LINES=$(printf '%s\n' "$PLAIN" | grep -cE '^(INFO|WARN|ERROR|DEBUG|TRACE) \(' || true)

if [ -n "$FILTER" ]; then
  SHOWN=$(printf '%s\n' "$PLAIN" | grep -aE "$FILTER" || true)
else
  SHOWN=$(printf '%s\n' "$PLAIN" | grep -aE '^(INFO|WARN|ERROR|DEBUG|TRACE) \(' || true)
fi

printf '%s\n' "$SHOWN" | awk -F'[()]' '
  NF > 1 { t = $2 + 0; if (seen) printf "%-72s (+%d ms)\n", $0, t - prev; else print $0; seen = 1; prev = t; next }
  { print }
'

echo
echo "--- ${APP_LINES} application lines, full log: ${LOG}"

# A panic is easy to miss among the noise, so call it out.
if printf '%s\n' "$PLAIN" | grep -qiE 'panic|stack overflow'; then
  echo "--- PANIC detected:"
  printf '%s\n' "$PLAIN" | grep -iA 6 'panic' | head -12
  exit 1
fi

if [ "$APP_LINES" -eq 0 ]; then
  cat >&2 <<'EOF'

No application output at all.

The bootloader may still have printed, which means the program is probably
running and talking to the other serial port. This board exposes two:

  espflash list-ports --list-all-ports

Logs go out of UART0, the CH343 "USB Single Serial" port, not the Espressif
"USB JTAG/serial debug unit" one. See .claude/skills/flash-and-verify/.
EOF
  exit 1
fi
