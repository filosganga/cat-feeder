#!/usr/bin/env python3
"""The command line of every dev script, bash or Python, parsed in one place,
so a flag behaves the same in all of them. Not meant to be run by hand.

A Python tool builds a Parser from its own docstring, which is its --help, and
adds options as it would to argparse:

    import _args
    ap = _args.Parser(__doc__)
    ap.add_argument("--layout", default="pcb.diy")
    args = ap.parse_args()

A bash script describes its options here and evaluates what comes back: one
assignment for each setting the command line gave, every value quoted. A
setting the command line did not give is not assigned, so set defaults first.

    ARGS=$(python3 "$DEV_DIR/_args.py" "$SCRIPT" \\
      --value port=PORT_ARG --flag reset=RESET --positional SECONDS \\
      -- "$@") || exit $?
    eval "$ARGS"

    --value a[,b]=VAR    --a takes a value, --a v or --a=v; --b is the same
    --flag a[,b]=VAR     --a takes none; VAR=1 when given
    --positional VAR     a bare argument, into the first VAR not yet set by an
                         option or a bare argument before it; repeat, in order.
                         An option after it overrides it, as in a bash loop
    --list VAR           the remaining bare arguments, into the array VAR
    --forward VAR        unknown options, each with the word after it unless
                         that is an option too, into the array VAR
    --refuse a=MESSAGE   --a is refused with MESSAGE
    --alternatives a,b   at most one of --a and --b, once

A bash script's --help is its header comment, from line 2 to the first empty
line, read from $SCRIPT, which _common.sh sets.

What every script gets, whichever language it is in:

- the word after an option that takes a value is its value, even if it starts
  with a dash, as `--password -x` must be; `--a=v` works too;
- an empty value is a typo, not a request for the default: `--port=` fails;
- no abbreviations: `--sec` is not `--seconds`;
- errors start with the script's name and exit 2: unknown option, unexpected
  argument, a missing value. Exit 1 is left for "it ran and the thing failed".

Only the standard library, and nothing newer than Python 3.9, which is what a
Mac without pyenv or Homebrew has: every bash script runs this, so it must at
least get far enough to say what it needs. A tool that needs more says so with
need_python. It never reads stdin, which a script may be holding a password on.
"""

import argparse
import os
import platform
import re
import shlex
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def die(message, prog=None):
    """A usage error: the message, after the script's name, and exit 2."""
    print(f"{prog or Path(sys.argv[0]).name}: {message}", file=sys.stderr)
    sys.exit(2)


def need_python(major, minor, why):
    if sys.version_info < (major, minor):
        die(f"needs Python {major}.{minor} or later for {why}; this is Python {platform.python_version()}")


def to_repo_root():
    """Every dev script runs from the repo root, wherever it was called from."""
    os.chdir(REPO)


def need_file(path, what):
    # Paths given relative to wherever this was called from now point
    # somewhere else, since the script has moved to the repo root. Say so
    # rather than report a missing file that plainly exists.
    if not Path(path).is_file():
        die(f"no {what} at {path} (paths are relative to the repo root)")


def _given(convert):
    def check(value):
        if value == "":
            raise argparse.ArgumentTypeError("needs a value")
        return convert(value) if convert else value

    check.__name__ = getattr(convert, "__name__", "str")
    return check


class _Help(argparse.Action):
    def __call__(self, parser, namespace, values, option_string=None):
        if parser.shell:
            print(f"printf '%s\\n' {shlex.quote(parser.help_text)}")
            print("exit 0")
        else:
            print(parser.help_text)
        sys.exit(0)


class _Alternative(argparse.Action):
    """An option that, with the others of its group, may be given once."""

    def __init__(self, *args, group, **kwargs):
        self.group = group
        super().__init__(*args, **kwargs)

    def __call__(self, parser, namespace, values, option_string=None):
        seen = parser.seen.get(self.group)
        if seen is self:
            parser.die(f"{self.option_strings[0]} given twice.")
        if seen is not None:
            names = [a.option_strings[0] for a in parser._actions if getattr(a, "group", None) == self.group]
            parser.die(f"{', '.join(names[:-1])} and {names[-1]} are alternatives; pass one.")
        parser.seen[self.group] = self
        setattr(namespace, self.dest, values)


class Parser(argparse.ArgumentParser):
    def __init__(self, help_text, prog=None, shell=False):
        super().__init__(prog=prog or Path(sys.argv[0]).name, add_help=False, allow_abbrev=False)
        self.help_text = help_text.strip("\n")
        self.shell = shell
        self.refused = {}
        self.seen = {}
        self._groups = 0
        self.add_argument("-h", "--help", action=_Help, nargs=0)

    def add_argument(self, *args, **kwargs):
        action = super().add_argument(*args, **kwargs)
        if action.option_strings and action.nargs is None:
            action.type = _given(action.type)
        return action

    def alternatives(self, *specs):
        """Options of which at most one is given, once: each spec is the
        positional arguments of one add_argument call."""
        self._groups += 1
        for names in specs:
            self.add_argument(*names, action=_Alternative, group=self._groups)

    def refuse(self, name, message):
        self.refused[name] = message

    def die(self, message):
        die(message, self.prog)

    def stray(self, arg):
        if arg.startswith("-") and arg != "-":
            self.die(f"unknown option '{arg}'. Try --help.")
        self.die(f"unexpected argument '{arg}'. Try --help.")

    def error(self, message):
        missing = re.match(r"argument (\S+): (expected one argument|needs a value)$", message)
        if missing:
            self.die(f"{missing.group(1).split('/')[0]} needs a value")
        extra = re.match(r"argument (\S+): ignored explicit argument ", message)
        if extra:
            self.die(f"{extra.group(1).split('/')[0]} takes no value. Try --help.")
        self.die(f"{re.sub(r'^argument ', '', message)}. Try --help.")

    def _joined(self, argv):
        """--a v as --a=v, for every --a that takes a value, so a value that
        starts with a dash is still a value; and refused options refused."""
        takes = {s for a in self._actions if a.nargs is None for s in a.option_strings}
        out, i = [], 0
        while i < len(argv):
            arg = argv[i]
            if arg == "--":
                out += argv[i:]
                break
            name = arg.split("=", 1)[0]
            if name in self.refused:
                self.die(self.refused[name])
            if arg in takes and i + 1 < len(argv):
                out.append(f"{arg}={argv[i + 1]}")
                i += 2
            else:
                out.append(arg)
                i += 1
        return out

    def parse_known(self, argv=None):
        """The options, and what is left: unknown options and stray words."""
        self.seen = {}
        argv = sys.argv[1:] if argv is None else list(argv)
        return self.parse_known_intermixed_args(self._joined(argv))

    def parse_args(self, argv=None):
        namespace, extras = self.parse_known(argv)
        if extras:
            self.stray(extras[0])
        return namespace


def header(script):
    """A bash script's header comment, from line 2 to the first empty line."""
    lines = Path(script).read_text().splitlines()[1:]
    out = []
    for line in lines:
        out.append(re.sub(r"^# ?", "", line))
        if line == "":
            break
    return "\n".join(out)


_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*$")


def _bound(item, flag):
    names, eq, var = item.partition("=")
    if not eq or not names or not _NAME.match(var):
        raise ValueError(f"{flag} {item!r}: expected name=VAR")
    return ["--" + n for n in names.split(",")], var


def _var(item, flag):
    if not _NAME.match(item):
        raise ValueError(f"{flag} {item!r}: not a shell variable name")
    return item


def shell(argv):
    """Parse a bash script's command line by its spec; print shell to eval."""
    if len(argv) < 2 or "--" not in argv:
        die("usage: _args.py SCRIPT [spec...] -- [arguments...]", "_args.py")
    script = argv[0]
    cut = argv.index("--")
    spec, args = argv[1:cut], argv[cut + 1:]
    prog = Path(script).name

    s = argparse.ArgumentParser(prog="_args.py", add_help=False, allow_abbrev=False)
    for flag in ("--value", "--flag", "--positional", "--list", "--forward", "--refuse", "--alternatives"):
        s.add_argument(flag, action="append", default=[])
    try:
        spec, extra = s.parse_known_args(spec)
        if extra:
            raise ValueError(f"unknown spec {extra[0]!r}")
        values = [_bound(v, "--value") for v in spec.value]
        flags = [_bound(f, "--flag") for f in spec.flag]
        positionals = [_var(p, "--positional") for p in spec.positional]
        lists = [_var(v, "--list") for v in spec.list]
        forwards = [_var(v, "--forward") for v in spec.forward]
        groups = [["--" + n for n in g.split(",")] for g in spec.alternatives]
        declared = {names[0] for names, _ in values}
        for name in (n for g in groups for n in g):
            if name not in declared:
                raise ValueError(f"--alternatives names {name}, which is no --value")
        if len(lists) > 1 or len(forwards) > 1:
            raise ValueError("at most one --list and one --forward")
        if forwards and (positionals or lists):
            raise ValueError("--forward cannot be combined with bare arguments")
    except ValueError as e:
        die(f"bad spec in {script}: {e}", "_args.py")

    p = Parser(header(script), prog=prog, shell=True)
    for item in spec.refuse:
        name, _, message = item.partition("=")
        p.refuse("--" + name, message)
    grouped = {n: g for g in groups for n in g}
    by_group = {}
    for names, var in values:
        if names[0] in grouped:
            by_group.setdefault(tuple(grouped[names[0]]), []).append((names, var))
        else:
            p.add_argument(*names, dest=var)
    for members in by_group.values():
        p._groups += 1
        for names, var in members:
            p.add_argument(*names, dest=var, action=_Alternative, group=p._groups)
    for names, var in flags:
        p.add_argument(*names, dest=var, action="store_true")
    if positionals or lists:
        p.add_argument("_bare", nargs="*")

    namespace, extras = p.parse_known(args)

    forwarded = []
    i = 0
    while i < len(extras):
        arg = extras[i]
        if not forwards or not arg.startswith("-"):
            p.stray(arg)
        forwarded.append(arg)
        if i + 1 < len(extras) and not extras[i + 1].startswith("-"):
            forwarded.append(extras[i + 1])
            i += 1
        i += 1

    # argparse has checked the command line; the values are read off it in
    # order, as the loops this replaces did. A bare word fills the first
    # positional nothing has set yet, and a later option overrides it, so
    # `flash --seconds 5 feed:` filters on feed:, and `soak 3 --hours 4` runs
    # four hours. argparse alone would apply every option before any bare word.
    var_of = {n: var for names, var in values for n in names}
    known = {s for a in p._actions for s in a.option_strings}
    assigned, rest, bare_only = {}, [], False
    for arg in p._joined(args):
        if not bare_only and arg == "--":
            bare_only = True
        elif not bare_only and arg.startswith("-") and arg != "-":
            name, eq, value = arg.partition("=")
            if eq and name in var_of:
                assigned[var_of[name]] = value
            elif name not in known and not forwards:
                # argparse reads -5 as a number, not an option, and lets it
                # through as a bare word; the loops refused it, and so does this.
                p.stray(arg)
        elif forwards:
            pass  # checked above: a word here is an unknown option's value
        elif any(v not in assigned for v in positionals):
            assigned[next(v for v in positionals if v not in assigned)] = arg
        elif lists:
            rest.append(arg)
        else:
            p.stray(arg)

    out = [f"{var}={shlex.quote(value)}" for var, value in assigned.items()]
    out += [f"{var}=1" for _, var in flags if getattr(namespace, var)]
    for var, items in ((lists[0] if lists else None, rest), (forwards[0] if forwards else None, forwarded)):
        if var and items:
            out.append(f"{var}=({' '.join(shlex.quote(a) for a in items)})")
    print("\n".join(out))


if __name__ == "__main__":
    shell(sys.argv[1:])
