#!/usr/bin/env bash
# Draws the perfboard as an SVG, without opening DIY Layout Creator.
#
#   ./dev/pcb-render.sh                       # pcb.diy -> target/pcb.svg
#   ./dev/pcb-render.sh --out board.svg       # somewhere else
#   ./dev/pcb-render.sh --layout other.diy    # another drawing
#   ./dev/pcb-render.sh --netlist nets.toml   # named by another netlist
#   ./dev/pcb-render.sh --side solder         # the bottom, as held to solder it
#   ./dev/pcb-render.sh --true-size           # to print at 100% and lay on the board
#
# The picture is what dev/pcb-check.sh believes, drawn from the same parser:
# holes and bottom traces coloured by net, jumpers, parts, header pin names and
# module outlines, with every net listed beside the board under its name in
# dev/netlist.toml, and anything the two disagree on in red. Open it in any
# browser; hovering a hole names it, its pin and its net. It is not DIYLC's own
# drawing, which only its File menu exports.
#
# --side solder mirrors the board, as it is when turned over to solder the
# bottom: column A on the right, the copper in front, the parts behind it faint.
# Hole names are the board's own, which read the same from either side. Its
# default output is target/pcb-solder.svg, so both views can be open at once.
#
# --true-size gives the page in millimetres: printed at 100%, with no fit to
# page, the holes are 2.54 mm apart and the picture lies over the board. The
# legend moves under the board and its text grows, so it reads on paper.
#
# The default output is under target/, which git ignores, so a picture of the
# board never lands in a commit by accident. Rerun it after the drawing is
# saved: DIYLC writes the file only on save.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

LAYOUT="pcb.diy"
OUT=""
NETLIST="dev/netlist.toml"
SIDE="component"
TRUE_SIZE=()

while [ $# -gt 0 ]; do
  case "$1" in
    --layout) need_value "$1" "${2:-}"; LAYOUT="$2"; shift 2 ;;
    --layout=*) LAYOUT="${1#*=}"; need_value --layout "$LAYOUT"; shift ;;
    --out) need_value "$1" "${2:-}"; OUT="$2"; shift 2 ;;
    --out=*) OUT="${1#*=}"; need_value --out "$OUT"; shift ;;
    --netlist) need_value "$1" "${2:-}"; NETLIST="$2"; shift 2 ;;
    --netlist=*) NETLIST="${1#*=}"; need_value --netlist "$NETLIST"; shift ;;
    --side) need_value "$1" "${2:-}"; SIDE="$2"; shift 2 ;;
    --side=*) SIDE="${1#*=}"; need_value --side "$SIDE"; shift ;;
    --true-size) TRUE_SIZE=(--true-size); shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "$(prog): unknown option '$1'. Try --help." ;;
    *) die "$(prog): unexpected argument '$1'. Try --help." ;;
  esac
done

# Same reason as pcb-check.sh: the script has moved to the repo root, so a path
# relative to wherever it was called from now points somewhere else.
[ -f "$LAYOUT" ] || die "$(prog): no layout at $LAYOUT (paths are relative to the repo root)"
[ -f "$NETLIST" ] || die "$(prog): no netlist at $NETLIST (paths are relative to the repo root)"
case "$SIDE" in
  component|solder) ;;
  *) die "$(prog): --side is component or solder, not '$SIDE'" ;;
esac

# Each view has its own default file, so rendering one never overwrites the
# other: pcb.svg, pcb-solder.svg, and -print for the true-size ones.
if [ -z "$OUT" ]; then
  OUT="target/pcb"
  [ "$SIDE" = solder ] && OUT="$OUT-solder"
  [ ${#TRUE_SIZE[@]} -gt 0 ] && OUT="$OUT-print"
  OUT="$OUT.svg"
fi

# The check's parser imports tomllib, which arrived in Python 3.11.
python3 -c 'import sys, tomllib' 2>/dev/null \
  || die "$(prog): needs Python 3.11 or later for tomllib; this is $(python3 --version 2>&1)"

exec python3 "$DEV_DIR/pcb-render.py" --layout "$LAYOUT" --out "$OUT" --netlist "$NETLIST" --side "$SIDE" ${TRUE_SIZE[@]+"${TRUE_SIZE[@]}"}
