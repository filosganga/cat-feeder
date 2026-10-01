#!/usr/bin/env bash
# Printable labels for units: the setup network, its password, and a QR code
# that joins it, so the case carries everything needed to get back into a unit.
#
#   ./dev/label.sh                    # read the id off the board that is plugged in
#   ./dev/label.sh 9a6ecc 99177c ...  # or name the ids, one label each
#   ./dev/label.sh --port /dev/cu.usbmodemXXXX
#   ./dev/label.sh --out labels.html  # somewhere other than a temporary file
#
# --port wins over ESPFLASH_PORT; see dev/_common.sh.
#
# Writes one HTML page and opens it; print it from the browser at 100% scale.
# Each label is 50 × 30 mm.
#
# The password comes from dev/ap-password.sh rather than being derived again
# here, so the derivation still exists in exactly two places — that script and
# `provisioning::ap_password` — and a label cannot disagree with the unit.
#
# The QR code is the standard Wi-Fi one (`WIFI:T:WPA;S:…;P:…;;`): a phone camera
# offers to join the network. That network only exists while the unit is in
# setup mode, so the code is for re-provisioning, not for everyday use. The
# admin page on the house network takes any username and the same password.
# The label prints the setup address, 192.168.4.1; it is `AP_ADDR_OCTETS` in
# provisioning.rs, so change it there and here together.
#
# ⚠️ The page holds each unit's password in the clear. By default it goes to a
# temporary file, not the working tree; with --out, keep it out of git.
#
# Needs qrencode: `brew install qrencode`.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

PORT_ARG=""
OUT=""
ids=()

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --out) need_value "$1" "${2:-}"; OUT="$2"; shift 2 ;;
    --out=*) OUT="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "label: unknown option '$1'. Try --help." ;;
    *) ids+=("$1"); shift ;;
  esac
done

if ! command -v qrencode >/dev/null 2>&1; then
  die "label: qrencode not found. Install it with: brew install qrencode"
fi

# Same arguments, same meaning: ids if given, otherwise the board on --port.
ap_args=()
[ -n "$PORT_ARG" ] && ap_args+=(--port "$PORT_ARG")
pairs=$("$DEV_DIR/ap-password.sh" ${ap_args[@]+"${ap_args[@]}"} ${ids[@]+"${ids[@]}"})

if [ -z "$OUT" ]; then
  OUT="$(mktemp -t cat-feeder-labels).html"
fi

# The Wi-Fi QR format reserves \ ; , : and " — escape them, although neither an
# SSID of ours nor a Crockford password can contain one today.
wifi_escape() {
  printf '%s' "$1" | sed -e 's/[\\;,:"]/\\&/g'
}

{
  cat <<'HTML'
<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>cat-feeder labels</title>
<style>
  @page { margin: 10mm; }
  body { font: 10pt/1.25 -apple-system, "Helvetica Neue", Arial, sans-serif; color: #000; background: #fff; margin: 0; }
  .sheet { display: flex; flex-wrap: wrap; gap: 4mm; }
  .label { width: 50mm; height: 30mm; box-sizing: border-box; border: 0.2mm dashed #999;
           padding: 2mm; display: flex; gap: 2mm; align-items: center; break-inside: avoid; }
  .qr svg { width: 24mm; height: 24mm; display: block; }
  .text { display: flex; flex-direction: column; gap: 1mm; min-width: 0; }
  .ssid { font-weight: 600; font-size: 7pt; word-break: break-all; }
  .pw { font: 700 9pt/1 ui-monospace, Menlo, monospace; letter-spacing: 0.02em; }
  .hint { font-size: 5.5pt; color: #333; }
  @media screen { body { padding: 10mm; } }
</style></head><body><div class="sheet">
HTML

  while read -r ssid password; do
    [ -n "$ssid" ] || continue
    qr=$(qrencode -t SVG --inline -m 1 -o - \
      "WIFI:T:WPA;S:$(wifi_escape "$ssid");P:$(wifi_escape "$password");;")
    cat <<HTML
<div class="label">
  <div class="qr">$qr</div>
  <div class="text">
    <div class="ssid">$ssid</div>
    <div class="pw">$password</div>
    <div class="hint">Setup: scan to join, then http://192.168.4.1</div>
    <div class="hint">Admin page: any user, this password</div>
  </div>
</div>
HTML
  done <<<"$pairs"

  echo '</div></body></html>'
} >"$OUT"

echo "labels: $OUT" >&2
if command -v open >/dev/null 2>&1; then
  open "$OUT"
fi
