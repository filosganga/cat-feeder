---
name: dev-script
description: The conventions every script in this project's dev/ directory follows — how it takes its arguments, where its settings come from, what its errors look like, and how it is verified before being called done. Use when adding a script to dev/, when changing how an existing one is invoked or configured, when reviewing one, or when a script needs a new setting.
---

# Writing a dev script

The scripts in `dev/` are the entire developer interface to this project. They
are consistent on purpose: a flag that means one thing in `flash` means the
same thing in `provision`, so nobody has to read a script to call it.

`dev/_common.sh` holds the shared parts and states the precedence rule. Source
it; do not reimplement it.

## The rule that matters most

**Every setting has a flag, and the flag wins.** Three sources, in order:

1. a flag — `--port`, `--board`, `--host`
2. an environment variable — `ESPFLASH_PORT`, `BOARD`, `MQTT_HOST`
3. a file or a default — `.cargo/config.toml`, `cfg.toml`, the literal default

The environment stays supported because a port is usually the same all day and
`export` says so once. But an `ENV=value ./dev/x` prefix changes the *start*
of the command line, which is what a permission rule in `.claude/settings.json`
matches on — so an allow-rule for the script stops covering the call and it
prompts. Prefer the flag when invoking, and always offer one.

## The skeleton

```bash
#!/usr/bin/env bash
# One line saying what this does.
#
#   ./dev/thing                    # the common case
#   ./dev/thing --port /dev/cu.usbmodemXXXX
#
# --port wins over ESPFLASH_PORT; see dev/_common.sh.

set -euo pipefail

DEV_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DEV_DIR/.."
# shellcheck source=dev/_common.sh
. "$DEV_DIR/_common.sh"
```

The header comment **is** the usage: `--help` prints it back, so there is one
copy rather than two that drift. It runs from line 2 to the first empty line,
which is why that blank line before `set -euo pipefail` is structural.

`DEV_DIR` is captured **before** the `cd`, because `$0` may be relative to
wherever the script was invoked from.

The parse loop, the flag names to reuse, and the full annotated template are in
[references/anatomy.md](references/anatomy.md).

## A Python tool

The command-line rules live in `dev/_args.py` (ADR-0031), and a Python tool
uses them rather than argparse directly. Its docstring is its `--help`:

```python
#!/usr/bin/env python3
"""One line saying what this does.

  ./dev/thing                    # the common case
  ./dev/thing --layout other.diy
"""

import sys

sys.dont_write_bytecode = True  # importing _args would leave a __pycache__ in dev/
import _args  # noqa: E402

_args.need_python(3, 11, "tomllib")  # only if it needs more than 3.9


def main():
    ap = _args.Parser(__doc__)
    ap.add_argument("--layout", default="pcb.diy")
    args = ap.parse_args()
    _args.to_repo_root()
    _args.need_file(args.layout, "layout")
    ...
    return 0  # 1 when it ran and the thing failed


if __name__ == "__main__":
    sys.exit(main())
```

Imports that need the newer Python go below `need_python`, or the one-line
refusal never gets to print. `_args.py` itself stays within Python 3.9, which
is what a Mac without pyenv has, and CI runs its tests there too; the repo's
`.python-version` is 3.11, the floor the pcb tools need, so working on it
catches anything newer.

`_args.py` takes a spec from bash too: `--value`, `--flag`, `--positional`,
`--list`, `--forward`, `--refuse`, `--alternatives`, described in its
docstring and tested, with today's scripts, in `dev/_args_test.py`. The bash
scripts still carry the loop in anatomy.md; moving them onto it is on the
roadmap.

## Rules

- **A script meant to be run has no extension** — `dev/flash`, not
  `dev/flash.sh` — so a caller never needs to know its language. A helper that
  is only sourced or called by other scripts starts with `_` and keeps its
  extension (`_common.sh`, `_render.sh`, `_tap.py`, `_args.py`, `_pcb.py`).
- **Reuse the flag names.** `--port`, `--board`, `--host`, `--user`,
  `--password`, `--nvs-offset`, `--seconds`, `--filter`, `--hours`, `--log`. A
  synonym for one of these is a bug.
- **Accept `--flag value` and `--flag=value`**, and `-h|--help`. An unknown
  `-*` is an error, never a positional.
- **`--flag` with nothing after it is a typo, not a request for the default.**
  `need_value` says so.
- **Keep positionals that already exist.** `./dev/flash 90 'feed:'` predates
  the flags and still works. A *new* script does not need positionals.
- **Exit 2 when it was called wrong, 1 when it ran and the thing failed.**
  `die` exits 2.
- **Errors are prefixed with the script's name** — `flash: ...`. `die` and
  `need_value` do this for you.
- **Comment why, not what.** These scripts carry the facts that cost a bench
  session to learn — why `provision` erases before writing, why no capture
  passes `--no-reset`. That is the house style and it is load-bearing: strip
  those comments and the mistakes come back.
- **Secrets go in `mktemp` with a `trap cleanup EXIT`**, never into the working
  tree, and are never echoed.
- **Serial captures render through `_render.sh`** rather than printing raw, and
  never pass `--no-reset` — it halts the application and captures only the
  bootloader.

## Finishing

1. `chmod +x`, then `bash -n` on every bash script:
   `grep -l '^#!/usr/bin/env bash' dev/* | xargs -n1 bash -n`. After any
   change to `_args.py`, `python3 dev/_args_test.py`.
2. Run `--help` from the repo root, from `dev/`, and from `/tmp`. All three
   must work; two of them catch the `$0` mistake above.
3. Exercise the error paths — unknown flag, missing value, one positional too
   many — and check flag-beats-env for at least one setting.
4. Document it in `dev/README.md`. If it becomes part of the everyday loop, add
   it to the Toolchain section of `CLAUDE.md` too.
5. If it is read-only, add `Bash(./dev/<name> *)` to the allow-list in
   `.claude/settings.json`. Anything that writes flash, overwrites a file or
   runs for hours stays prompting.
