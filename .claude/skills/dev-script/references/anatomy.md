# Anatomy of a dev script

- [The command line](#the-command-line)
- [Flag names already in use](#flag-names-already-in-use)
- [What `_common.sh` gives you](#what-_commonsh-gives-you)
- [Resolving a setting](#resolving-a-setting)
- [Forwarding options to another program](#forwarding-options-to-another-program)
- [Working with espflash](#working-with-espflash)
- [A complete script](#a-complete-script)
- [Checklist](#checklist)

## The command line

A script describes its options to `dev/_args.py` and evaluates the
assignments it prints. Each variable starts empty and gets its default
*after*, so "not given" and "given the default" stay distinguishable:

```bash
PORT_ARG=""
SECONDS_TO_CAPTURE=""
FILTER=""

# The command line, parsed by dev/_args.py (ADR-0031).
ARGS=$(python3 "$DEV_DIR/_args.py" "$SCRIPT" \
  --value port=PORT_ARG --value seconds=SECONDS_TO_CAPTURE --value filter=FILTER \
  --positional SECONDS_TO_CAPTURE --positional FILTER \
  -- "$@") || exit $?
eval "$ARGS"

SECONDS_TO_CAPTURE="${SECONDS_TO_CAPTURE:-45}"
```

| Spec | Means |
|---|---|
| `--value a[,b]=VAR` | `--a` takes a value, `--a v` or `--a=v`; `--b` is another spelling |
| `--flag a[,b]=VAR` | `--a` takes none; `VAR=1` when given |
| `--positional VAR` | a bare word fills the first such `VAR` nothing has set yet; a later option overrides it. Only for scripts that already had positionals |
| `--list VAR` | every other bare word, into the array `VAR`; `--` lets one start with `-` |
| `--forward VAR` | unknown options, with the word after each, into the array `VAR` |
| `--refuse a=MESSAGE` | `--a`, however spelled, exits 2 with `MESSAGE` |
| `--alternatives a,b` | at most one of `--a` and `--b`, once |

A list-valued positional, such as topics in `watch` or device ids in
`ap-password`, arrives as a plain array; build what the program needs from it
afterwards:

```bash
topics=()
for t in ${TOPICS[@]+"${TOPICS[@]}"}; do topics+=(-t "$t"); done
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
| `prog` | the script's name, for messages |
| `die "msg"` | print to stderr, exit 2 |
| `SCRIPT` | the calling script's absolute path, which `_args.py` reads the header from |
| `resolve_port "$PORT_ARG"` | flag, then `ESPFLASH_PORT`, then `.cargo/config.toml`; may be empty |
| `require_port "$PORT_ARG"` | the same, but dies with the `list-ports` hint if empty |

`SCRIPT` depends on `DEV_DIR` being set before the source line, and says so
loudly if it is not.

Use `resolve_port` when an empty answer is a legitimate outcome the script
handles itself — `ap-password` only needs a port if no device id was named,
and its error mentions both ways out. Use `require_port` otherwise.

## Resolving a setting

One line, after the command line is parsed, per setting:

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

Forwarding is the default, and it is why `provision` needs no change when
`mkrecord` grows an option — a typo comes back as `mkrecord`'s own usage rather
than as a guess from the shell. `--forward MKRECORD_ARGS` collects each
unknown option, with the word after it unless that is an option too.

Expand a possibly-empty array as `${ARR[@]+"${ARR[@]}"}`; a bare
`"${ARR[@]}"` is an unbound-variable error under `set -u` on bash 3.2, which is
still what `/bin/bash` is on macOS.

The exceptions earn their place one at a time. `provision` names `--port`,
`--host`, `--user`, `--password`, `--password-file` and `--nvs-offset` because
each has an `ESPFLASH_PORT` / `MQTT_*` / `NVS_OFFSET` variable behind it, and a
forwarded flag would reach `mkrecord` intact while skipping that layer
entirely — so `MQTT_USER=x ./dev/provision` would silently do nothing.
Everything else, `--detent-ms` and `--portion-scale` included, is forwarded.

Naming a flag costs something, so do not do it for free: `provision` parses
`--password` and `--password-file` and therefore has to enforce that they are
alternatives itself (`--alternatives password,password-file`), because
`mkrecord`'s own check never sees a duplicate that the shell already
collapsed.

Reject a forwarded option that would break the script's own contract —
`provision` refuses `--out`, because the record is a temporary file it
deletes on the way out, and redirecting it would leave the Wi-Fi password
somewhere nobody cleans up: `--refuse 'out=...'`.

## Working with espflash

- Always `--port "$PORT"`. The dev kit exposes two ports and espflash picks the
  wrong one.
- Always `--non-interactive`, so nothing waits for a human.
- **Never `--no-reset`.** It loads a flash stub that halts the application, and
  the capture then contains bootloader output only. That mistake costs a whole
  run and looks like a dead program.
- Hand the capture to `_render.py`, which runs it, keeps everything in the
  log, shows each line as it arrives with the gap since the previous one, and
  stops it at the deadline or at Ctrl-C. It then flags a panic and exits
  non-zero when the application printed nothing at all. Not `timeout(1)`: it
  puts its command in a process group of its own, which Ctrl-C never reaches.

```bash
exec python3 "$DEV_DIR/_render.py" --as thing --log "$LOG" --seconds "$SECONDS_TO_CAPTURE" \
  -- espflash monitor --non-interactive --port "$PORT"
```

  `--filter RE` and `--listening` go before the `--`. Without `--seconds` and
  a command, it renders a finished log, which is what `dev/flash` does.

## A complete script

`dev/capture` is the shortest one that uses every convention: header comment
as usage, `DEV_DIR` before the `cd`, a spec with preserved positionals,
`require_port`, and a capture run by `_render.py` at the end. Read it before
writing a new one.

## Checklist

- [ ] Named `dev/<name>`, no extension; `#!/usr/bin/env bash`, `set -euo pipefail`, `chmod +x`.
- [ ] Header comment shows the common case and every flag, then one blank line.
- [ ] `DEV_DIR` captured before `cd "$DEV_DIR/.."`, then `_common.sh` sourced.
- [ ] Every setting has a flag; the flag wins over the variable.
- [ ] Flag names match the table above.
- [ ] The command line parsed by `_args.py`, no loop of its own; its spec and
      cases added to `dev/_args_test.py`.
- [ ] Messages prefixed with the script name, exit 2 for misuse.
- [ ] Secrets in `mktemp` with `trap cleanup EXIT`; nothing secret echoed.
- [ ] `bash -n` clean; `--help` works from the repo root, `dev/` and `/tmp`.
- [ ] Error paths and flag-beats-env exercised by hand.
- [ ] `dev/README.md` updated; `CLAUDE.md` too if it joins the everyday loop.
- [ ] An allow-rule in `.claude/settings.json` if it is read-only.
