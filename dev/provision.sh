#!/usr/bin/env bash
# Write this board's settings straight into flash, once, so the firmware never
# has to carry them.
#
#   ./dev/provision.sh                          # the cfg.toml values
#   ./dev/provision.sh --detent-ms 900          # ...with a measured interval
#   ./dev/provision.sh --portion-scale 133      # ...and a measured portion size
#   ./dev/provision.sh --host auto              # ...this machine, whatever its address
#   ./dev/provision.sh --host <broker>          # ...some other broker
#   ./dev/provision.sh --port /dev/cu.usbmodemXXXX
#   ./dev/provision.sh --nvs-offset 0x9000      # if the firmware reports another
#
# Pointing a unit at a broker that is not the dev stack usually means different
# credentials too, and they need no second config file:
#
#   ./dev/provision.sh --host <broker> --user <name> --password-file <path>
#   pass show mqtt/feeder | ./dev/provision.sh --host <broker> --password-file -
#
# --port, --host, --user, --password and --nvs-offset win over ESPFLASH_PORT,
# MQTT_HOST, MQTT_USER, MQTT_PASS and NVS_OFFSET; see dev/_common.sh. Every
# other option is handed to `mkrecord`, which is what validates it — run
# `cargo run --example mkrecord -- --help`.
#
# ⚠️ --password is visible in `ps` and in shell history. For a broker that
# matters use --password-file, or `--password-file -` and pipe it in.
#
# `--host auto`, or `mqtt_host = "auto"` in cfg.toml, resolves to this machine's
# LAN address at provisioning time. The firmware has no resolver — mqtt.rs parses
# the stored value with Ipv4Addr::from_str — so a name can never be stored, and
# a hand-written address goes stale the next time DHCP moves the Mac.
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
HOST_ARG=""
USER_ARG=""
PASS_ARG=""
PASS_FILE_ARG=""
PASS_SEEN=""
MKRECORD_ARGS=()

# The password may be given exactly once, by exactly one spelling.
#
# Enforced here rather than left to `mkrecord`, whose own check this script
# makes unreachable: it resolves the two flags into one before forwarding, so a
# duplicate never reaches it. Silently taking the last would be the usual shell
# convention and the wrong one here — the outcome is a unit that joins the
# Wi-Fi and is refused by the broker, which is red ×2 on the LED and looks
# exactly like a broker that is down.
pass_once() {
  [ -z "$PASS_SEEN" ] || {
    if [ "$PASS_SEEN" = "$1" ]; then
      die "provision: $1 given twice."
    else
      die "provision: --password and --password-file are alternatives; pass one."
    fi
  }
  PASS_SEEN="$1"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --host) need_value "$1" "${2:-}"; HOST_ARG="$2"; shift 2 ;;
    --host=*) HOST_ARG="${1#*=}"; shift ;;
    # Named here rather than forwarded blindly, so they pick up MQTT_USER and
    # MQTT_PASS the way every other setting picks up its variable. The spelling
    # matches dev/watch.sh exactly, including MQTT_PASS rather than
    # MQTT_PASSWORD, so one broker's credentials work with both scripts.
    --user) need_value "$1" "${2:-}"; USER_ARG="$2"; shift 2 ;;
    --user=*) USER_ARG="${1#*=}"; shift ;;
    --password|--pass) pass_once --password; need_value "$1" "${2:-}"; PASS_ARG="$2"; shift 2 ;;
    --password=*|--pass=*) pass_once --password; PASS_ARG="${1#*=}"; shift ;;
    --password-file) pass_once --password-file; need_value "$1" "${2:-}"; PASS_FILE_ARG="$2"; shift 2 ;;
    --password-file=*) pass_once --password-file; PASS_FILE_ARG="${1#*=}"; shift ;;
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

# The broker's address: --host, then MQTT_HOST, then cfg.toml's own value.
#
# `auto` in any of the three means this machine's LAN address, which is what the
# dev stack actually lives at. It has to be resolved *here* rather than in the
# firmware, because there is no resolver on the device: `mqtt.rs` parses this
# with `Ipv4Addr::from_str`, so only a literal address can ever work.
#
# Worth the machinery because the failure is so quiet. A Mac's DHCP lease moves,
# every unit provisioned before the move keeps the old address, and each one
# sits flashing red twice — which is exactly right for "no broker" and looks
# identical to a broker that is down. Cost a session here.
HOST="${HOST_ARG:-${MQTT_HOST:-$(awk -F'"' '/^[[:space:]]*mqtt_host/{print $2}' cfg.toml 2>/dev/null)}}"
HOST="$(resolve_broker_host "$HOST")"
if [ -n "$HOST" ]; then
  MKRECORD_ARGS+=(--host "$HOST")
fi

# The broker's credentials, same three sources. Unset is not the same as empty:
# when neither the flag nor the variable is set, nothing is forwarded and
# `mkrecord` falls back to cfg.toml — which keeps the config file as the single
# place credentials are parsed, rather than teaching this script to read TOML.
MQTT_USER="${USER_ARG:-${MQTT_USER:-}}"
if [ -n "$MQTT_USER" ]; then
  MKRECORD_ARGS+=(--user "$MQTT_USER")
fi

if [ -n "$PASS_FILE_ARG" ]; then
  # Forwarded as a path, so the secret never becomes an argument. `-` means
  # stdin and is passed straight through — `cargo run` leaves stdin connected,
  # so a pipe into this script reaches mkrecord unbroken.
  MKRECORD_ARGS+=(--password-file "$PASS_FILE_ARG")
else
  MQTT_PASS="${PASS_ARG:-${MQTT_PASS:-}}"
  if [ -n "$MQTT_PASS" ]; then
    MKRECORD_ARGS+=(--password "$MQTT_PASS")
  fi
fi

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
