#!/usr/bin/env bash
# Summarise an overnight capture from dev/soak.sh.
#
#   ./dev/soak-report.sh soak/2026-09-15_2040.log
#   ./dev/soak-report.sh                          # the most recent one
#
# Answers, in order: did it stay up, did it feed, and did anything go wrong.

set -uo pipefail

cd "$(dirname "$0")/.."

LOG="${1:-$(ls -t soak/*.log 2>/dev/null | head -1)}"
if [ -z "$LOG" ] || [ ! -f "$LOG" ]; then
  echo "No soak log. Run ./dev/soak.sh first." >&2
  exit 2
fi

app() { grep -aE '^(INFO|WARN|ERROR|DEBUG|TRACE) \(' "$LOG"; }

echo "=== $LOG"
echo "    $(app | wc -l | tr -d ' ') application lines, $(du -h "$LOG" | cut -f1) on disk"
echo

# A reboot mid-soak is the single most important thing to notice, and it is
# easy to miss in thousands of lines: the log simply starts again.
BOOTS=$(grep -ac "Embassy initialized" "$LOG" || true)
echo "=== boots: ${BOOTS}"
echo "    1 is expected — the monitor resets the board when it attaches."
echo "    More means it restarted. Check the rst: lines below for why."
grep -a "rst:0x" "$LOG" | sed 's/^/    /' || true
echo

echo "=== panics, backtraces, allocation failures"
if grep -aiqE 'panic|stack overflow|out of memory|alloc' "$LOG"; then
  grep -aiE -A 6 'panic|stack overflow|out of memory|alloc' "$LOG" | head -40 | sed 's/^/    /'
else
  echo "    none"
fi
echo

echo "=== scheduled feeds"
grep -aE 'schedule: slot' "$LOG" | sed 's/^/    /' || echo "    none"
echo

echo "=== feeds that ran"
grep -aE 'feed: (start|done)' "$LOG" | sed 's/^/    /' || echo "    none"
echo

echo "=== jams"
grep -ac 'jammed' "$LOG" | sed 's/^/    /'
echo "    (expected once per feed while no motor is connected)"
echo

echo "=== connection"
echo "    wifi disconnects:  $(grep -ac 'wifi: disconnected' "$LOG" || true)"
echo "    mqtt reconnects:   $(grep -ac 'mqtt: connected' "$LOG" || true)"
echo "    mqtt failures:     $(grep -acE 'mqtt: (disconnected|connect failed|tcp connect failed|publish to)' "$LOG" || true)"
echo

# The schedule does not run until a live feeder/time arrives, so a unit still
# holding at the end of the log fed nothing all night and said so only here.
echo "=== is the schedule armed?"
if grep -aq 'schedule armed' "$LOG"; then
  grep -a 'schedule armed' "$LOG" | head -3 | sed 's/^/    /'
else
  echo "    NO. The schedule never armed, so nothing was ever going to feed."
  echo "    A live feeder/time never arrived: check Home Assistant is running"
  echo "    and its publish-the-time automation is enabled."
  grep -aE 'clock: (started|no trusted)' "$LOG" | head -3 | sed 's/^/    /'
fi
echo

# Sub-2s drift is not logged at all, so anything here is worth a look. Steady
# growth in one direction is crystal drift; isolated large values are the
# broker or Home Assistant hiccuping.
echo "=== clock alignments worth noticing"
grep -aE 'clock: (started|aligned|ignored)' "$LOG" | tail -20 | sed 's/^/    /' || echo "    none"
echo

echo "=== warnings and errors, by kind"
app | grep -aE '^(WARN|ERROR)' \
  | sed -E 's/^[A-Z]+ \([0-9]+\) - //; s/[0-9]+/N/g' \
  | sort | uniq -c | sort -rn | head -15 | sed 's/^/    /' || echo "    none"
