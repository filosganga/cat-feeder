#!/usr/bin/env bash
# Build the firmware and send it to a unit over the network (ADR-0022).
#
#   ./dev/ota.sh --address 192.168.1.123 --id a1b2c3 --board zero --headless
#   ./dev/ota.sh --address 192.168.1.123 --id a1b2c3          # a dev kit
#
# --address is the unit's own IP (its admin page), not the broker's: --host
# means the broker in every other script. --id is the unit's id, from which
# the admin password is derived exactly as dev/ap-password.sh derives it.
# --board wins over BOARD; see dev/_common.sh.
#
# --board and --headless must describe the unit, as for dev/flash.sh: the
# image is accepted whatever it was built for, and a dev-kit image on a Zero
# boots with no console and no network — the bootloader then rolls it back
# after CONFIRM_SECS (src/bin/main.rs), but that is a unit doing nothing.
#
# The unit checks the image before selecting it (update.rs): the trailing
# SHA-256 `espflash save-image` appends, and a mark of this cfg.toml's
# ap_secret, so an image built with another secret, or before OTA support, is
# refused rather than locking the admin page. It then restarts into the new
# image, which confirms itself once it reaches the broker; one that does not
# within CONFIRM_SECS is rolled back by the bootloader (ADR-0024).
#
# Do not reset the unit until it has confirmed: dev/capture.sh, dev/flash.sh
# and every espflash command reset it, and a reset before the confirmation is
# exactly what rolls the update back. Watch the broker instead.
#
# A unit still on the old single-slot partition table answers that it has no
# OTA slots: it needs one USB flash with dev/flash.sh first.
#
# Release build: what a unit in a feeder should run.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

ADDRESS=""
ID=""
BOARD_ARG=""
HEADLESS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --address) need_value "$1" "${2:-}"; ADDRESS="$2"; shift 2 ;;
    --address=*) ADDRESS="${1#*=}"; shift ;;
    --id) need_value "$1" "${2:-}"; ID="$2"; shift 2 ;;
    --id=*) ID="${1#*=}"; shift ;;
    --board) need_value "$1" "${2:-}"; BOARD_ARG="$2"; shift 2 ;;
    --board=*) BOARD_ARG="${1#*=}"; shift ;;
    --headless) HEADLESS=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "ota: unknown option '$1'. Try --help." ;;
    *) die "ota: unexpected argument '$1'. Try --help." ;;
  esac
done

[ -n "$ADDRESS" ] || die "ota: --address <unit ip> is required. Try --help."
[ -n "$ID" ] || die "ota: --id <unit id> is required. Try --help."

BOARD="${BOARD_ARG:-${BOARD:-devkit}}"
case "$BOARD" in
  devkit) BOARD_FLAGS=(--features "board-devkit${HEADLESS:+,headless}") ;;
  zero) BOARD_FLAGS=(--no-default-features --features "board-zero${HEADLESS:+,headless}") ;;
  *) die "ota: --board must be 'devkit' or 'zero', not '$BOARD'" ;;
esac

PASSWORD=$("$DEV_DIR/ap-password.sh" "$ID" | awk '{print $2}')
[ -n "$PASSWORD" ] || { echo "ota: could not derive the password for $ID" >&2; exit 1; }

IMAGE=$(mktemp "${TMPDIR:-/tmp}/cat-feeder-ota.XXXXXX")
CURL_CONFIG=$(mktemp "${TMPDIR:-/tmp}/cat-feeder-ota-auth.XXXXXX")
cleanup() { rm -f "$IMAGE" "$CURL_CONFIG"; }
trap cleanup EXIT

echo "building for ${BOARD}${HEADLESS:+, headless} (release)..."
cargo build --release ${BOARD_FLAGS[@]+"${BOARD_FLAGS[@]}"}

# espflash.toml supplies the partition table, so this also refuses an image
# too large for a slot before anything is sent.
espflash save-image --chip esp32c6 \
  target/riscv32imac-unknown-none-elf/release/cat-feeder "$IMAGE" >/dev/null
echo "image: $(wc -c <"$IMAGE" | tr -d ' ') bytes"

# The password goes in a config file, not on the command line, where `ps`
# would show it. Any username: the admin page checks only the password.
printf 'user = "admin:%s"\n' "$PASSWORD" >"$CURL_CONFIG"

echo "sending to http://${ADDRESS}/update ..."
# No Origin header: curl sends none, which the admin page accepts from a
# client that brings the password (ADR-0017).
if ! curl --silent --show-error --fail-with-body --config "$CURL_CONFIG" \
  --header 'Content-Type: application/octet-stream' \
  --data-binary @"$IMAGE" "http://${ADDRESS}/update"; then
  echo
  echo "ota: the unit refused the update, or could not be reached" >&2
  exit 1
fi
echo
echo "Watch it come back with ./dev/watch.sh 'feeder/${ID}/#'. Not dev/capture.sh:"
echo "a reset before the new image confirms itself rolls it back."
