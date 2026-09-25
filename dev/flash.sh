#!/usr/bin/env bash
# Build, flash, and capture the serial console for a fixed window.
#
#   ./dev/flash.sh                          # flash, then capture 45 s
#   ./dev/flash.sh --seconds 90             # flash, then capture 90 s
#   ./dev/flash.sh --filter 'feed:|motor:'  # ...showing only matching lines
#   ./dev/flash.sh --board zero             # ...a Zero rather than the dev kit
#   ./dev/flash.sh --port /dev/cu.usbmodemXXXX
#
# The first two are also positional, as they always were: `./dev/flash.sh 90
# 'feed:'` still works.
#
# Each flag has a matching environment variable — BOARD, ESPFLASH_PORT — and the
# flag wins. See dev/_common.sh for why both exist.
#
# --board picks the feature set, and it is not cosmetic: it also decides which
# interface `esp-println` writes to. A dev-kit binary on a Zero compiles, flashes
# and boots, then prints nothing at all, because it is talking to a UART while
# the Zero's only console is the chip's own USB. That looks exactly like a dead
# application or a wrong port. Set it to match the board in your hand.
#
# The two boards also enumerate as different serial ports, so --port usually has
# to change with --board:
#
#   espflash list-ports --list-all-ports
#
# Prints every line with the gap since the previous one, which is what turns a
# wall of log into a readable sequence. The full capture is kept, and its path
# printed, so a filtered view never loses the original.
#
# Exits non-zero if the application produced no output at all, which is the
# classic "wrong serial port" symptom rather than a dead program.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

SECONDS_TO_CAPTURE=""
FILTER=""
BOARD_ARG=""
PORT_ARG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --board) need_value "$1" "${2:-}"; BOARD_ARG="$2"; shift 2 ;;
    --board=*) BOARD_ARG="${1#*=}"; shift ;;
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --seconds) need_value "$1" "${2:-}"; SECONDS_TO_CAPTURE="$2"; shift 2 ;;
    --seconds=*) SECONDS_TO_CAPTURE="${1#*=}"; shift ;;
    --filter) need_value "$1" "${2:-}"; FILTER="$2"; shift 2 ;;
    --filter=*) FILTER="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "flash: unknown option '$1'. Try --help." ;;
    *)
      if [ -z "$SECONDS_TO_CAPTURE" ]; then SECONDS_TO_CAPTURE="$1"
      elif [ -z "$FILTER" ]; then FILTER="$1"
      else die "flash: unexpected argument '$1'. Try --help."
      fi
      shift
      ;;
  esac
done

SECONDS_TO_CAPTURE="${SECONDS_TO_CAPTURE:-45}"

# Cargo features are additive, so selecting `board-zero` means *replacing* the
# default rather than adding to it. Without --no-default-features both board
# features end up on and esp-println's build script rejects the pair.
BOARD="${BOARD_ARG:-${BOARD:-devkit}}"
case "$BOARD" in
  devkit) BOARD_FLAGS=() ;;
  zero) BOARD_FLAGS=(--no-default-features --features board-zero) ;;
  *) die "flash: --board must be 'devkit' or 'zero', not '$BOARD'" ;;
esac

PORT="$(require_port "$PORT_ARG")"

BIN=target/riscv32imac-unknown-none-elf/debug/cat-feeder
LOG=$(mktemp "${TMPDIR:-/tmp}/cat-feeder-flash.XXXXXX")

echo "building for ${BOARD}..."
cargo build ${BOARD_FLAGS[@]+"${BOARD_FLAGS[@]}"}

echo "flashing and capturing ${SECONDS_TO_CAPTURE}s on ${PORT}"
echo

# --non-interactive: never prompt, so this works unattended.
# No --no-reset: it loads a flash stub that halts the application, and the
# console then shows the bootloader only. That mistake costs a whole run.
timeout "$((SECONDS_TO_CAPTURE + 60))" espflash flash \
  --monitor --non-interactive --chip esp32c6 --port "$PORT" "$BIN" \
  >"$LOG" 2>&1 || true

exec "$DEV_DIR/_render.sh" "$LOG" "$FILTER"
