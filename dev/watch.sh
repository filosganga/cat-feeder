#!/usr/bin/env bash
# Tails everything the firmware and Home Assistant say to each other.
#
#   ./dev/watch.sh                  # feeder/# and homeassistant/#
#   ./dev/watch.sh 'feeder/#'       # just one filter
#
# Against another broker — a production Home Assistant, say — name it, and give
# it credentials too if that broker's feeder user differs from the dev stack's:
#
#   ./dev/watch.sh --host <broker> 'feeder/time'
#   ./dev/watch.sh --host <broker> --user <name> --password-file <path> 'feeder/#'
#
# --password takes the secret inline and lands in `ps` and shell history;
# --password-file reads it from a file, or from stdin when given `-`. They are
# alternatives, and the spelling matches dev/provision.sh so the same password
# reaches the check and the provisioning the same way.
#
# MQTT_HOST, MQTT_USER and MQTT_PASS still work; the flags win over them. See
# dev/_common.sh for why both exist.
#
# Ctrl+C to stop.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"

HOST_ARG=""
USER_ARG=""
PASS_ARG=""
PASS_FILE_ARG=""
PASS_SEEN=""
topics=()

# Once, by one spelling — the same rule provision.sh enforces, for the same
# reason: a silently-ignored password reads as an authentication failure on the
# broker rather than as a mistake on the command line.
pass_once() {
  [ -z "$PASS_SEEN" ] || {
    if [ "$PASS_SEEN" = "$1" ]; then
      die "watch: $1 given twice."
    else
      die "watch: --password and --password-file are alternatives; pass one."
    fi
  }
  PASS_SEEN="$1"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --host) need_value "$1" "${2:-}"; HOST_ARG="$2"; shift 2 ;;
    --host=*) HOST_ARG="${1#*=}"; shift ;;
    --user) need_value "$1" "${2:-}"; USER_ARG="$2"; shift 2 ;;
    --user=*) USER_ARG="${1#*=}"; shift ;;
    --password|--pass) pass_once --password; need_value "$1" "${2:-}"; PASS_ARG="$2"; shift 2 ;;
    --password=*|--pass=*) pass_once --password; PASS_ARG="${1#*=}"; shift ;;
    # Same spelling as provision.sh, because this is the command that checks a
    # deployed broker before a unit is pointed at it, and having to retype the
    # password differently for the check and for the provisioning is how one of
    # the two ends up wrong.
    --password-file) pass_once --password-file; need_value "$1" "${2:-}"; PASS_FILE_ARG="$2"; shift 2 ;;
    --password-file=*) pass_once --password-file; PASS_FILE_ARG="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; while [ $# -gt 0 ]; do topics+=(-t "$1"); shift; done ;;
    -*) die "watch: unknown option '$1'. Try --help." ;;
    *) topics+=(-t "$1"); shift ;;
  esac
done

if [ -n "$PASS_FILE_ARG" ]; then
  # The first line is the password, with a trailing \r dropped for CRLF files.
  # `IFS= read -r` gives the first line and strips nothing else, which is
  # mkrecord's rule exactly — see read_password in examples/mkrecord.rs. They
  # have to agree: this command is the pre-flight check that is supposed to
  # catch a wrong password *before* it is written into a unit, and a check that
  # reads the file differently fails on passwords that would have worked.
  if [ "$PASS_FILE_ARG" = "-" ]; then
    IFS= read -r PASS_ARG || true
  else
    [ -r "$PASS_FILE_ARG" ] || die "watch: cannot read $PASS_FILE_ARG"
    IFS= read -r PASS_ARG < "$PASS_FILE_ARG" || true
  fi
  PASS_ARG="${PASS_ARG%$'\r'}"
  [ -n "$PASS_ARG" ] || die "watch: $PASS_FILE_ARG has no password on its first line"
fi

MQTT_HOST="${HOST_ARG:-${MQTT_HOST:-}}"

# Flag, then environment, then cfg.toml — the same chain `provision.sh` uses,
# and the reason this command can be called a credential test at all. It is
# documented as the check that catches a wrong password *before* a unit is
# pointed at a broker, which only holds if it authenticates the way the unit
# will. Falling straight to the dev-stack literals would have meant
# `./dev/watch.sh --host <broker>` quietly trying feeder/feeder-dev while
# `./dev/provision.sh --host <broker>` in the same shell used cfg.toml's.
#
# The literals stay as the last resort, which is what the local stack gets when
# there is no cfg.toml yet.
cfg_value() {
  [ -f cfg.toml ] || return 0
  awk -F'"' -v k="$1" '$0 ~ "^[[:space:]]*" k "[[:space:]]*=" {print $2; exit}' cfg.toml
}

MQTT_USER="${USER_ARG:-${MQTT_USER:-$(cfg_value mqtt_user)}}"
MQTT_PASS="${PASS_ARG:-${MQTT_PASS:-$(cfg_value mqtt_password)}}"
MQTT_USER="${MQTT_USER:-feeder}"
MQTT_PASS="${MQTT_PASS:-feeder-dev}"

if [ ${#topics[@]} -eq 0 ]; then
  topics=(-t 'feeder/#' -t 'homeassistant/#')
fi

# The local dev stack is watched from inside its own container, so nothing has
# to be installed here and the password file is the one already mounted.
if [ -z "$MQTT_HOST" ]; then
  exec docker compose exec mosquitto \
    mosquitto_sub -h localhost -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
fi

# Another broker. Prefer a mosquitto_sub on the PATH, so watching a deployed
# instance does not require the dev stack to be running — which is the whole
# point of having deployed. Fall back to the container, which reaches the LAN
# as well.
if command -v mosquitto_sub >/dev/null 2>&1; then
  exec mosquitto_sub -h "$MQTT_HOST" -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
fi

echo "watch: no local mosquitto_sub; going through the dev stack's container." >&2
echo "       brew install mosquitto to avoid needing it running." >&2
exec docker compose exec mosquitto \
  mosquitto_sub -h "$MQTT_HOST" -u "$MQTT_USER" -P "$MQTT_PASS" -v "${topics[@]}"
