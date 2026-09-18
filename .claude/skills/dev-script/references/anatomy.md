# Anatomy of a dev script

- [The parse loop](#the-parse-loop)
- [Flag names already in use](#flag-names-already-in-use)
- [What `_common.sh` gives you](#what-_commonsh-gives-you)
- [Resolving a setting](#resolving-a-setting)
- [Forwarding options to another program](#forwarding-options-to-another-program)
- [Working with espflash](#working-with-espflash)
- [A complete script](#a-complete-script)
- [Checklist](#checklist)

## The parse loop

Copy this shape. It handles `--flag value` and `--flag=value`, rejects an
unknown option instead of treating it as a positional, and leaves the defaults
to be applied afterwards — so "not given" and "given the default" stay
distinguishable while parsing.

```bash
PORT_ARG=""
SECONDS_TO_CAPTURE=""
FILTER=""

while [ $# -gt 0 ]; do
  case "$1" in
    --port) need_value "$1" "${2:-}"; PORT_ARG="$2"; shift 2 ;;
    --port=*) PORT_ARG="${1#*=}"; shift ;;
    --seconds) need_value "$1" "${2:-}"; SECONDS_TO_CAPTURE="$2"; shift 2 ;;
    --seconds=*) SECONDS_TO_CAPTURE="${1#*=}"; shift ;;
    --filter) need_value "$1" "${2:-}"; FILTER="$2"; shift 2 ;;
    --filter=*) FILTER="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "thing: unknown option '$1'. Try --help." ;;
    *)
      # Only if this script already had positionals. New ones need none.
      if [ -z "$SECONDS_TO_CAPTURE" ]; then SECONDS_TO_CAPTURE="$1"
      elif [ -z "$FILTER" ]; then FILTER="$1"
      else die "thing: unexpected argument '$1'. Try --help."
      fi
      shift
      ;;
  esac
done

SECONDS_TO_CAPTURE="${SECONDS_TO_CAPTURE:-45}"
```

A list-valued positional — topics in `watch.sh`, device ids in
`ap-password.sh` — collects into an array instead, and supports `--` so a value
that starts with `-` can still be passed:

```bash
topics=()
# ...in the case:
    --) shift; while [ $# -gt 0 ]; do topics+=(-t "$1"); shift; done ;;
    *) topics+=(-t "$1"); shift ;;
# ...after the loop:
if [ ${#topics[@]} -eq 0 ]; then
  topics=(-t 'feeder/#' -t 'homeassistant/#')
fi
```

## Flag names already in use

Reuse these. Inventing `--serial` for `--port`, or `--broker` for `--host`,
makes the set unlearnable.

| Flag | Environment variable | Falls back to |
|---|---|---|
| `--port` | `ESPFLASH_PORT` | `ESPFLASH_PORT` in `.cargo/config.toml` |
| `--board` | `BOARD` | `devkit` |
| `--host` | `MQTT_HOST` | empty, meaning the local Docker stack |
| `--user` | `MQTT_USER` | `feeder` |
| `--password` (alias `--pass`) | `MQTT_PASS` | `feeder-dev` |
| `--password-file` | — | none; reads the file, or stdin given `-` |
| `--nvs-offset` | `NVS_OFFSET` | `0x9000` |
| `--seconds` | — | 45 |
| `--filter` | — | every application line |
| `--hours` | — | 12 |
| `--log` | — | the newest file in `soak/` |

The last four are per-run rather than ambient, which is why they have no
variable. A new per-run setting does not need one either.

## What `_common.sh` gives you

| Function | Does |
|---|---|
| `prog` | the script's name without `.sh`, for messages |
| `die "msg"` | print to stderr, exit 2 |
| `need_value "$1" "${2:-}"` | die unless the flag was given a value |
| `usage` | print the calling script's header comment (uses `SCRIPT`) |
| `resolve_port "$PORT_ARG"` | flag, then `ESPFLASH_PORT`, then `.cargo/config.toml`; may be empty |
| `require_port "$PORT_ARG"` | the same, but dies with the `list-ports` hint if empty |

`usage` depends on `DEV_DIR` being set before the source line, and says so
loudly if it is not.

Use `resolve_port` when an empty answer is a legitimate outcome the script
handles itself — `ap-password.sh` only needs a port if no device id was named,
and its error mentions both ways out. Use `require_port` otherwise.

## Resolving a setting

One line, after the parse loop, per setting:

```bash
NVS_OFFSET="${OFFSET_ARG:-${NVS_OFFSET:-0x9000}}"
MQTT_HOST="${HOST_ARG:-${MQTT_HOST:-}}"
BOARD="${BOARD_ARG:-${BOARD:-devkit}}"
```

Shadowing the environment variable with the resolved value is deliberate: the
rest of the script then reads one name and cannot accidentally use the
unresolved one.

## Forwarding options to another program

**The rule: name a flag here only if it has an environment variable; forward
everything else.**

Forwarding is the default, and it is why `provision.sh` needs no change when
`mkrecord` grows an option — a typo comes back as `mkrecord`'s own usage rather
than as a guess from the shell:

```bash
    -*)
      MKRECORD_ARGS+=("$1")
      shift
      if [ $# -gt 0 ] && [ "${1#-}" = "$1" ]; then
        MKRECORD_ARGS+=("$1")
        shift
      fi
      ;;
```

Expand a possibly-empty array as `${ARR[@]+"${ARR[@]}"}`; a bare
`"${ARR[@]}"` is an unbound-variable error under `set -u` on bash 3.2, which is
still what `/bin/bash` is on macOS.

The exceptions earn their place one at a time. `provision.sh` names `--port`,
`--host`, `--user`, `--password`, `--password-file` and `--nvs-offset` because
each has an `ESPFLASH_PORT` / `MQTT_*` / `NVS_OFFSET` variable behind it, and a
forwarded flag would reach `mkrecord` intact while skipping that layer
entirely — so `MQTT_USER=x ./dev/provision.sh` would silently do nothing.
Everything else, `--detent-ms` and `--portion-scale` included, is forwarded.

Naming a flag costs something, so do not do it for free: `provision.sh` parses
`--password` and `--password-file` and therefore has to enforce that they are
alternatives itself, because `mkrecord`'s own check never sees a duplicate that
the shell already collapsed.

Reject a forwarded option that would break the script's own contract —
`provision.sh` refuses `--out`, because the record is a temporary file it
deletes on the way out, and redirecting it would leave the Wi-Fi password
somewhere nobody cleans up.

## Working with espflash

- Always `--port "$PORT"`. The dev kit exposes two ports and espflash picks the
  wrong one.
- Always `--non-interactive`, so nothing waits for a human.
- **Never `--no-reset`.** It loads a flash stub that halts the application, and
  the capture then contains bootloader output only. That mistake costs a whole
  run and looks like a dead program.
- Bound a capture with `timeout`, let it fail with `|| true`, and hand the log
  to `_render.sh` — which annotates each line with the gap since the previous
  one, flags a panic, and exits non-zero when the application printed nothing
  at all.

```bash
timeout "$SECONDS_TO_CAPTURE" espflash monitor \
  --non-interactive --port "$PORT" >"$LOG" 2>&1 || true

exec "$DEV_DIR/_render.sh" "$LOG" "$FILTER"
```

## A complete script

`dev/capture.sh` is the shortest one that uses every convention: header comment
as usage, `DEV_DIR` before the `cd`, a parse loop with preserved positionals,
`require_port`, a bounded capture, and `_render.sh` at the end. Read it before
writing a new one.

## Checklist

- [ ] `#!/usr/bin/env bash`, `set -euo pipefail`, `chmod +x`.
- [ ] Header comment shows the common case and every flag, then one blank line.
- [ ] `DEV_DIR` captured before `cd "$DEV_DIR/.."`, then `_common.sh` sourced.
- [ ] Every setting has a flag; the flag wins over the variable.
- [ ] Flag names match the table above.
- [ ] `--flag=value` handled next to `--flag value`, and `-h|--help` present.
- [ ] Unknown `-*` dies; a stray positional dies.
- [ ] Messages prefixed with the script name, exit 2 for misuse.
- [ ] Secrets in `mktemp` with `trap cleanup EXIT`; nothing secret echoed.
- [ ] `bash -n` clean; `--help` works from the repo root, `dev/` and `/tmp`.
- [ ] Error paths and flag-beats-env exercised by hand.
- [ ] `dev/README.md` updated; `CLAUDE.md` too if it joins the everyday loop.
- [ ] An allow-rule in `.claude/settings.json` if it is read-only.
