#!/usr/bin/env bash
# The setup network's name and password for a unit, so a sticker can be printed
# before the unit is ever powered on.
#
#   ./dev/ap-password.sh              # read the id off the board that is plugged in
#   ./dev/ap-password.sh db0260 ...   # or name the ids
#
# With no arguments it asks the connected board for its MAC, which is the
# moment you are already holding the unit: flash it, run this, stick the label
# on. The id is the last three bytes of the MAC, the same derivation the
# firmware uses for its MQTT topics.
#
# Must agree exactly with `provisioning::ap_password` in the firmware. Both
# compute base32(sha256("<ap_secret>:<device id>")), taking sixty bits and
# Crockford's alphabet. If you change one, change the other, and expect every
# sticker already printed to be wrong.
#
# The device id is the last three bytes of the station MAC in lowercase hex, as
# printed at boot ("board: devkit, id=db0260") and by `espflash board-info`.

set -euo pipefail

cd "$(dirname "$0")/.."

if [ ! -f cfg.toml ]; then
  echo "cfg.toml not found. Copy cfg.toml.example and fill it in." >&2
  exit 2
fi

SECRET=$(awk -F'"' '/^[[:space:]]*ap_secret[[:space:]]*=/ {print $2}' cfg.toml)
if [ -z "$SECRET" ]; then
  cat >&2 <<'EOF'
No ap_secret in cfg.toml.

It is the one build-time secret this feature keeps, and it is what stops the
setup password being computable from the MAC the unit broadcasts. Generate one
once and keep it:

  echo "ap_secret    = \"$(openssl rand -hex 16)\"" >> cfg.toml
EOF
  exit 2
fi

ids=("$@")

if [ ${#ids[@]} -eq 0 ]; then
  PORT="${ESPFLASH_PORT:-}"
  if [ -z "$PORT" ]; then
    PORT=$(awk -F'"' '/^ESPFLASH_PORT=/ {print $2}' .cargo/config.toml)
  fi
  if [ -z "$PORT" ]; then
    echo "No board id given and no serial port to ask. Either:" >&2
    echo "  $0 db0260" >&2
    echo "  ESPFLASH_PORT=/dev/cu.usbmodemXXXX $0" >&2
    exit 2
  fi

  echo "reading the id from the board on ${PORT}..." >&2
  # espflash picks the wrong one of this board's two ports by default, hence
  # the explicit --port. The MAC is printed as a0:85:e3:db:02:60; the id is the
  # last three bytes, which is what the firmware uses for its topics.
  # Strip the label rather than splitting on ":", because the MAC is full of
  # colons and splitting yields just its first byte.
  mac=$(timeout 60 espflash board-info --port "$PORT" 2>&1 \
    | sed 's/\x1b\[[0-9;]*m//g' \
    | awk '/^MAC address:/ { sub(/^MAC address:[[:space:]]*/, ""); print; exit }')

  if [ -z "$mac" ]; then
    echo "Could not read the MAC. Is the board plugged in, and is the monitor closed?" >&2
    echo "Only one process can hold a serial port." >&2
    exit 1
  fi

  ids=("$(echo "$mac" | tr -d ':' | tr '[:upper:]' '[:lower:]' | tail -c 7)")
  echo "id: ${ids[0]}  (MAC ${mac})" >&2
fi

SECRET="$SECRET" python3 - "${ids[@]}" <<'PY'
import hashlib, os, sys

ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
secret = os.environ["SECRET"]

def password(device_id):
    digest = hashlib.sha256(f"{secret}:{device_id}".encode()).digest()
    bits = int.from_bytes(digest[:8], "big")
    out = ""
    for i in range(12):
        if i and i % 4 == 0:
            out += "-"
        out += ALPHABET[(bits >> (59 - 5 * i)) & 31]
    return out

for device_id in sys.argv[1:]:
    print(f"cat-feeder-{device_id}    {password(device_id)}")
PY
