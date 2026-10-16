# The dev scripts' command lines are parsed in Python, bash ones included

Every script in `dev/` follows the same rules for its command line: `--flag
value` and `--flag=value`, an empty value is a typo, errors name the script
and exit 2, `--help` prints the header. Each bash script carried its own copy
of the parse loop, and the copies drifted: an empty `--flag=` was accepted by
the nine bash scripts that take options and refused by the three pcb tools. The
Python tools had a second copy of their flags, in a bash wrapper in front of
each one.

So the rules live in one place, `dev/_args.py`, built on argparse. A Python
tool builds its parser from it. A bash script passes it a short description of
its options and the command line, and evaluates the assignments it prints, each
value quoted by `shlex.quote`. An error goes to stderr and the script exits
with its code.

What it costs:

- Every bash script needs `python3`. A machine that builds this firmware
  has one: `capture` already needs it, and so do the pcb tools.
- `_args.py` itself is kept to Python 3.9, which is what a Mac without pyenv
  or Homebrew has, so it can at least say what a tool needs; CI runs its tests
  on 3.9, 3.11 and the newest. The repo's `.python-version` names 3.11, which
  the pcb tools need for `tomllib`. For a pyenv user with no 3.11 installed,
  that file makes `python3` in the repo fail with pyenv's own message rather
  than ours.
- About 40 ms per call, and a call to `eval` in each bash script.
- argparse has to be held to the old behaviour: no abbreviations, the word
  after an option is its value even when it starts with a dash, so a password
  like `-x` still works, and a bash script's bare arguments are read in order,
  so a later option overrides one, as in the loops.

The alternative was a `_common.py` alongside `_common.sh`, with the same rules
written twice in two languages, and the bash loops left as they were. Neither
removes the copies, and the copies are what drifted.
