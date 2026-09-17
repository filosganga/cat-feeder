#!/usr/bin/env bash
# Write this board's settings straight into flash, once, so the firmware never
# has to carry them.
#
#   ./dev/provision.sh                          # the cfg.toml values
#   ./dev/provision.sh --detent-ms 900          # ...with a measured interval
#   ./dev/provision.sh --portion-scale 133      # ...and a measured portion size
#   ./dev/provision.sh --port /dev/cu.usbmodemXXXX
#   ./dev/provision.sh --nvs-offset 0x9000      # if the firmware reports another
#
# --port and --nvs-offset win over ESPFLASH_PORT and NVS_OFFSET; see
# dev/_common.sh. Every other option is handed to `mkrecord`, which is what
# validates it — run `cargo run --example mkrecord -- --help` for that list.
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
# An unprovisioned board says "no record yet, going to setup" instead and raises
# its own Wi-Fi network. There is no third outcome: nothing is compiled in.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

PORT_ARG=""
OFFSET_ARG=""
MKRECORD_ARGS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --nvs-offset) need_value "$1" "${2:-}"; OFFSET_ARG="$2"; shift 2 ;;
    --nvs-offset=*) OFFSET_ARG="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    # The record's path is this script's business: it is a temporary file that
    # is deleted on the way out, so letting it be redirected would leave the
    # Wi-Fi password somewhere nobody cleans up.
    --out|--out=*) die "provision: --out is not yours to set; the record is a temporary file." ;;
    # Everything else belongs to mkrecord. Forwarded rather than listed here, so
    # a new mkrecord option needs no change in this script — and a typo comes
    # back as mkrecord's own usage rather than as a guess from here.
    -*)
      MKRECORD_ARGS+=("$1")
      shift
      if [ $# -gt 0 ] && [ "${1#-}" = "$1" ]; then
        MKRECORD_ARGS+=("$1")
        shift
      fi
      ;;
    *) die "provision: unexpected argument '$1'. Try --help." ;;
  esac
done

# The default ESP-IDF table's nvs offset. The firmware finds its own through the
# partition table and prints the answer at boot:
#
#   store: nvs at 0x9000, 24576 bytes
#
# If that ever disagrees with this, believe the firmware and pass --nvs-offset.
NVS_OFFSET="${OFFSET_ARG:-${NVS_OFFSET:-0x9000}}"

HOST_TARGET="$(rustc -vV | awk '/^host:/{print $2}')"

if [ ! -f cfg.toml ]; then
  echo "provision: cfg.toml not found." >&2
  echo "           cp cfg.toml.example cfg.toml and fill it in." >&2
  exit 1
fi

PORT="$(require_port "$PORT_ARG")"

# The record holds the Wi-Fi password in the clear, so it is built in a private
# temporary file and removed on the way out rather than left in the working
# tree where it could be committed. mktemp, not a fixed name, so two runs cannot
# collide and nothing predictable is left behind.
RECORD="$(mktemp -t cat-feeder-record)"
cleanup() { rm -f "$RECORD"; }
trap cleanup EXIT

echo "building the record..."
cargo run --quiet --example mkrecord --target "$HOST_TARGET" -- \
  --out "$RECORD" ${MKRECORD_ARGS[@]+"${MKRECORD_ARGS[@]}"}

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
echo "    store: configured for ..."
echo "If it says \"no record yet, going to setup\" the write did not land."
