#!/usr/bin/env bash
# Shared by the dev scripts. Sourced, never run directly.
#
# ## Flags win over the environment
#
# Every setting these scripts take has three sources, in this order:
#
#   1. a command-line flag        --port, --board, --host, ...
#   2. an environment variable    ESPFLASH_PORT, BOARD, MQTT_HOST, ...
#   3. a file or a default        .cargo/config.toml, cfg.toml, the default
#
# The environment came first and stays, because a port or a broker address is
# usually the same all day and `export` says so once. But an env-var prefix
# changes the *start* of the command line, which is what a permission rule in
# .claude/settings.json matches on: `BOARD=zero ./dev/flash.sh` is not
# `./dev/flash.sh`, so an allow-rule for the script does not cover it. A flag
# leaves the command starting with the script's own path, so one rule covers
# every invocation — and it is less to type by hand.
#
# So: every environment variable here also has a flag, and the flag wins.

# The script's own name, without the .sh, so every message from these scripts
# reads the same: "flash: ...", never "flash.sh: ...".
prog() { basename "$0" .sh; }

die() {
  echo "$*" >&2
  exit 2
}

# `--foo` with nothing after it, or an empty value, is a typo rather than a
# request for the default — say so instead of silently using the default.
need_value() { # need_value <flag> <value>
  [ -n "${2:-}" ] || die "$(prog): $1 needs a value"
}

# The calling script's own path, absolute, because every script cds to the repo
# root before sourcing this and `$0` may be relative to wherever it was invoked
# from. Reading it back is what --help does.
SCRIPT="${DEV_DIR:?dev scripts must set DEV_DIR before sourcing _common.sh}/$(basename "$0")"

# --help prints the calling script's own header comment, so the usage has one
# copy rather than two that drift apart.
usage() {
  sed -n '2,/^$/p' "$SCRIPT" | sed 's/^#//; s/^ //'
}

# The serial port, from the flag, the environment, or .cargo/config.toml — the
# last of which is what `cargo run` already uses, so a board that works there
# works here without being told twice.
resolve_port() { # resolve_port [value of --port]
  if [ -n "${1:-}" ]; then
    printf '%s' "$1"
  elif [ -n "${ESPFLASH_PORT:-}" ]; then
    printf '%s' "$ESPFLASH_PORT"
  else
    awk -F'"' '/^ESPFLASH_PORT=/ {print $2}' .cargo/config.toml 2>/dev/null
  fi
}

# This machine's address on the LAN, which is what a feeder has to be told: the
# firmware has no resolver, so `mqtt.rs` parses `mqtt_host` with
# `Ipv4Addr::from_str` and a name would be stored, survive a reboot and never
# connect.
#
# It comes from DHCP and moves. When it does, every provisioned unit is pointing
# at an address that now belongs to something else — or to nothing, which is
# what happened here: a board sat flashing red twice, which is *correct*
# behaviour for "no broker" and looks exactly like a broken broker. Resolving it
# at provisioning time is what stops that being written into flash by hand.
#
# The default route's interface rather than a hardcoded en0, because that is
# Wi-Fi on one machine and Ethernet or a dock on the next.
lan_ip() {
  local iface ip
  iface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2}')"
  if [ -n "$iface" ]; then
    ip="$(ipconfig getifaddr "$iface" 2>/dev/null || true)"
    [ -n "$ip" ] && { printf '%s' "$ip"; return 0; }
  fi

  # Linux, and a macOS fallback if the route lookup found nothing usable.
  ip="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i=1;i<NF;i++) if ($i=="src") print $(i+1)}')"
  [ -n "$ip" ] && { printf '%s' "$ip"; return 0; }

  return 1
}

# `auto` anywhere a broker address is expected means "this machine".
#
# It is a literal in cfg.toml rather than the default, so that a file naming a
# real address still means that address. Only `auto` opts in to something that
# changes under you.
resolve_broker_host() { # resolve_broker_host <host>
  if [ "${1:-}" != "auto" ]; then
    printf '%s' "${1:-}"
    return 0
  fi

  lan_ip || die "$(prog): --host auto could not work out this machine's LAN address"
}

require_port() { # require_port [value of --port]
  local port
  port="$(resolve_port "${1:-}")"
  if [ -z "$port" ]; then
    echo "$(prog): no serial port. Pass --port, set ESPFLASH_PORT, or see:" >&2
    echo "  espflash list-ports --list-all-ports" >&2
    exit 2
  fi
  printf '%s' "$port"
}
