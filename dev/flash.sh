#!/usr/bin/env bash
# Build, flash, and capture the serial console for a fixed window.
#
#   ./dev/flash.sh                 # flash, then capture 45 s
#   ./dev/flash.sh 90              # flash, then capture 90 s
#   ./dev/flash.sh 60 'feed:|motor:'   # ...showing only matching lines
#
#   BOARD=zero ./dev/flash.sh      # ...a Zero rather than the dev kit
#
# BOARD picks the feature set, and it is not cosmetic: it also decides which
# interface `esp-println` writes to. A dev-kit binary on a Zero compiles, flashes
# and boots, then prints nothing at all, because it is talking to a UART while
# the Zero's only console is the chip's own USB. That looks exactly like a dead
# application or a wrong port. Set BOARD to match the board in your hand.
#
# The two boards also enumerate as different serial ports, so ESPFLASH_PORT
# usually has to change with BOARD:
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

cd "$(dirname "$0")/.."

SECONDS_TO_CAPTURE="${1:-45}"
FILTER="${2:-}"

# Cargo features are additive, so selecting `board-zero` means *replacing* the
# default rather than adding to it. Without --no-default-features both board
# features end up on and esp-println's build script rejects the pair.
BOARD="${BOARD:-devkit}"
case "$BOARD" in
  devkit) BOARD_FLAGS=() ;;
  zero) BOARD_FLAGS=(--no-default-features --features board-zero) ;;
  *)
    echo "flash: BOARD must be 'devkit' or 'zero', not '$BOARD'" >&2
    exit 2
    ;;
esac

# shellcheck disable=SC1090
PORT="${ESPFLASH_PORT:-}"
if [ -z "$PORT" ]; then
  # .cargo/config.toml sets this for cargo-run, but not for a bare shell.
  PORT=$(awk -F'"' '/^ESPFLASH_PORT=/ {print $2}' .cargo/config.toml)
fi
if [ -z "$PORT" ]; then
  echo "No serial port. Set ESPFLASH_PORT, or see:" >&2
  echo "  espflash list-ports --list-all-ports" >&2
  exit 2
fi

BIN=target/riscv32imac-unknown-none-elf/debug/cat-feeder
LOG=$(mktemp "${TMPDIR:-/tmp}/cat-feeder-flash.XXXXXX")

echo "building for ${BOARD}..."
cargo build "${BOARD_FLAGS[@]}"

echo "flashing and capturing ${SECONDS_TO_CAPTURE}s on ${PORT}"
echo

# --non-interactive: never prompt, so this works unattended.
# No --no-reset: it loads a flash stub that halts the application, and the
# console then shows the bootloader only. That mistake costs a whole run.
timeout "$((SECONDS_TO_CAPTURE + 60))" espflash flash \
  --monitor --non-interactive --chip esp32c6 --port "$PORT" "$BIN" \
  >"$LOG" 2>&1 || true

exec "$(dirname "$0")/_render.sh" "$LOG" "$FILTER"
