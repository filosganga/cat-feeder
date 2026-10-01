#!/usr/bin/env bash
# Checks the perfboard drawing before anything is soldered or powered.
#
#   ./dev/pcb-check.sh                        # pcb.diy against dev/pinouts.toml
#   ./dev/pcb-check.sh --all                  # ...and list signal-to-signal neighbours too
#   ./dev/pcb-check.sh --layout other.diy     # another drawing
#   ./dev/pcb-check.sh --pinouts parts.toml   # another set of part pinouts
#
# Reports each header's pin names against the part's own silkscreen order
# (dev/pinouts.toml), supplies joined to ground or to each other, the netlist
# and the pins connected to nothing, labels that disagree with their pin, and
# every pair of neighbouring holes on different nets, worst first.
#
# Exits 1 when a pinout, a supply or a label is wrong; neighbours and
# unconnected pins are reported for reading, since every perfboard has them.
#
# What it cannot know: anything the drawing does not say. A part missing from
# dev/pinouts.toml is reported as unchecked, not as fine, and a part this does
# not recognise is listed under "Not checked" rather than silently dropped.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

LAYOUT="pcb.diy"
PINOUTS="dev/pinouts.toml"
ALL=()

while [ $# -gt 0 ]; do
  case "$1" in
    --layout) need_value "$1" "${2:-}"; LAYOUT="$2"; shift 2 ;;
    --layout=*) LAYOUT="${1#*=}"; need_value --layout "$LAYOUT"; shift ;;
    --pinouts) need_value "$1" "${2:-}"; PINOUTS="$2"; shift 2 ;;
    --pinouts=*) PINOUTS="${1#*=}"; need_value --pinouts "$PINOUTS"; shift ;;
    --all) ALL=(--all); shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "$(prog): unknown option '$1'. Try --help." ;;
    *) die "$(prog): unexpected argument '$1'. Try --help." ;;
  esac
done

# Paths given relative to wherever this was called from would now point
# somewhere else, since the script has moved to the repo root. Say so rather
# than reporting a missing file that plainly exists.
[ -f "$LAYOUT" ] || die "$(prog): no layout at $LAYOUT (paths are relative to the repo root)"
[ -f "$PINOUTS" ] || die "$(prog): no pinouts at $PINOUTS (paths are relative to the repo root)"

# tomllib arrived in Python 3.11.
python3 -c 'import sys, tomllib' 2>/dev/null \
  || die "$(prog): needs Python 3.11 or later for tomllib; this is $(python3 --version 2>&1)"

exec python3 "$DEV_DIR/pcb-check.py" --layout "$LAYOUT" --pinouts "$PINOUTS" ${ALL[@]+"${ALL[@]}"}
