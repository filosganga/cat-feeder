#!/usr/bin/env bash
# Write this board's settings straight into flash, once, so the firmware never
# has to carry them.
#
#   ./dev/provision.sh                          # the cfg.toml values
#   ./dev/provision.sh --detent-ms 900          # ...with a measured interval
#   ./dev/provision.sh --portion-scale 133      # ...and a measured portion size
#
# The record lands in the `nvs` partition, which an application reflash never
# touches — so a board provisioned once keeps its settings across every
# `cargo run`, with nothing re-seeded at boot.
#
# This is also how the three production units get set up without ever raising an
# access point or typing a password on a phone.
#
# Afterwards the console should say
#
#   store: configured for <ssid> via <host>:<port>
#
# with no "seeded from cfg.toml" line after it. That absent line is the proof
# the credentials came from flash and not from the binary.

set -euo pipefail

cd "$(dirname "$0")/.."

# The default ESP-IDF table's nvs offset. The firmware finds its own through the
# partition table and prints the answer at boot:
#
#   store: nvs at 0x9000, 24576 bytes
#
# If that ever disagrees with this, believe the firmware and set NVS_OFFSET.
NVS_OFFSET="${NVS_OFFSET:-0x9000}"

HOST_TARGET="$(rustc -vV | awk '/^host:/{print $2}')"

if [ ! -f cfg.toml ]; then
  echo "provision: cfg.toml not found." >&2
  echo "           cp cfg.toml.example cfg.toml and fill it in." >&2
  exit 1
fi

PORT="${ESPFLASH_PORT:-}"
if [ -z "$PORT" ]; then
  PORT="$(awk -F'"' '/ESPFLASH_PORT/{print $2}' .cargo/config.toml 2>/dev/null || true)"
fi
if [ -z "$PORT" ]; then
  echo "provision: no serial port. Set ESPFLASH_PORT, or see:" >&2
  echo "           espflash list-ports --list-all-ports" >&2
  exit 1
fi

# The record holds the Wi-Fi password in the clear, so it is built in a private
# temporary file and removed on the way out rather than left in the working
# tree where it could be committed. mktemp, not a fixed name, so two runs cannot
# collide and nothing predictable is left behind.
RECORD="$(mktemp -t cat-feeder-record)"
cleanup() { rm -f "$RECORD"; }
trap cleanup EXIT

echo "building the record..."
cargo run --quiet --example mkrecord --target "$HOST_TARGET" -- \
  --out "$RECORD" "$@"

# Both espflash commands below hard-reset the chip when they finish, so the
# board boots between them and again at the end. That is harmless today because
# nothing in the firmware writes flash at boot — setup mode only saves a record
# when a form is submitted. If that ever changes, this script has a race in it.
#
# The same reset is why erasing a record and *then* flashing new firmware does
# not work: the old firmware gets a boot in which to write flash back. Flash
# first, erase second.
#
# Erase before writing, and not as a precaution: `write-bin` does NOT erase, and
# NOR flash can only clear bits. Writing a new record over an old one ANDs the
# two together — which was found the hard way, because `FDR2` written over
# `FDR1` becomes `FDR0` and the firmware reports "no record yet". It looks
# exactly like the write silently failing rather than like corruption.
#
# One 4 KB sector is enough: a record is a few hundred bytes, and erase-region
# requires a sector-aligned address anyway.
echo
echo "erasing one sector at $NVS_OFFSET on $PORT..."
espflash erase-region --port "$PORT" "$NVS_OFFSET" 0x1000

echo
echo "writing to $NVS_OFFSET..."
espflash write-bin --port "$PORT" "$NVS_OFFSET" "$RECORD"

echo
echo "done. Reflash the application and look for:"
echo "    store: configured for ... "
echo "and NOT:"
echo "    store: seeded from cfg.toml"
