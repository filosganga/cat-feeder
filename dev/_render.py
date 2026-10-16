#!/usr/bin/env python3
"""Renders a serial log: colour codes stripped, the application's lines (or
those matching --filter), each annotated with the gap since the one before,
then a summary. Used by dev/flash and dev/capture; not meant to be called
directly.

    _render.py --as NAME --log FILE [--filter RE] [--listening]
    _render.py --as NAME --log FILE [--filter RE] [--listening] --seconds N -- COMMAND...

NAME is the calling script's, so an error reads "capture: ...", not this
file's name.

The first renders a finished log. The second runs COMMAND, keeps everything it
prints in FILE as it arrives, and renders each line as it comes. It stops
COMMAND after N seconds (0: no limit) or at Ctrl-C, and either way finishes
as the first does: the summary, and the log kept. What it prints is what the
first would print for the same log, except that a --filter matching empty
lines shows the trailing ones the first drops.

--listening means the capture did not reset the unit, so silence is a quiet
unit, not a dead one or the wrong port.

The gap is read off the unit's own clock, the milliseconds in "INFO (1234) -",
not this machine's, so it says how long the firmware took however the bytes
were buffered on the way. It is computed as the awk this replaced computed it,
byte for byte. --filter is read by Python's re, which agrees with grep -E on
alternation, groups, bracket classes, anchors and repetition; of the POSIX
classes, [[:alpha:]] [[:digit:]] [[:alnum:]] [[:upper:]] [[:lower:]]
[[:space:]] [[:blank:]] [[:xdigit:]] and [[:punct:]] are translated, and any
other is refused rather than misread.
"""

import os
import re
import select
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True  # importing _args would leave a __pycache__ in dev/
import _args  # noqa: E402

COLOUR = re.compile(rb"\x1b\[[0-9;]*m")
APP = re.compile(rb"^(INFO|WARN|ERROR|DEBUG|TRACE) \(")
PANIC = re.compile(rb"panic|stack overflow", re.I)
PANIC_CONTEXT = re.compile(rb"panic", re.I)

# What awk's `$2 + 0` reads at the start of a field: strtod's syntax, hex
# included, after leading white space; nothing numeric reads as 0.
NUMBER = re.compile(
    rb"[ \t\n\v\f\r]*([-+]?)(?:0[xX]([0-9a-fA-F]+\.?[0-9a-fA-F]*|\.[0-9a-fA-F]+)([pP][-+]?[0-9]+)?"
    rb"|([0-9]+\.?[0-9]*(?:[eE][-+]?[0-9]+)?|\.[0-9]+(?:[eE][-+]?[0-9]+)?))"
)

POSIX_CLASSES = {
    "alpha": "a-zA-Z", "digit": "0-9", "alnum": "a-zA-Z0-9", "upper": "A-Z", "lower": "a-z",
    "space": r" \t\n\r\f\v", "blank": r" \t", "xdigit": "0-9A-Fa-f",
    "punct": r"!-/:-@\[-`{-~",
}

NO_OUTPUT = """
No application output at all.

The bootloader may still have printed, which means the program is probably
running and talking to the other serial port. This board exposes two:

  espflash list-ports --list-all-ports

Logs go out of UART0, the CH343 "USB Single Serial" port, not the Espressif
"USB JTAG/serial debug unit" one. See .claude/skills/flash-and-verify/.
"""


def awk_number(field):
    m = NUMBER.match(field)
    if not m:
        return 0.0
    sign, hex_digits, hex_exp, decimal = m.groups()
    if decimal is not None:
        value = float(decimal)
    else:
        value = float.fromhex("0x" + hex_digits.decode() + (hex_exp or b"p0").decode())
    return -value if sign == b"-" else value


def compile_filter(pattern, prog):
    for name, body in POSIX_CLASSES.items():
        pattern = pattern.replace(f"[:{name}:]", body)
    unknown = re.search(r"\[:(\w+):\]", pattern)
    if unknown:
        _args.die(f"--filter: [:{unknown.group(1)}:] is not a class this can read", prog)
    try:
        return re.compile(pattern.encode())
    except re.error as e:
        _args.die(f"--filter {pattern!r} is not a regular expression: {e}", prog)


class Lines:
    """Shows each line as it arrives, as the old grep and awk did together."""

    def __init__(self, keep):
        self.keep = keep
        self.seen = False
        self.prev = 0.0
        self.shown = 0

    def line(self, raw):
        plain = COLOUR.sub(b"", raw)
        if not self.keep(plain):
            return
        self.shown += 1
        if b"(" in plain or b")" in plain:
            t = awk_number(re.split(rb"[()]", plain, maxsplit=2)[1])
            if self.seen:
                plain = b"%-72s (+%d ms)" % (plain, int(t - self.prev))
            self.seen, self.prev = True, t
        write(plain + b"\n")

    def end(self):
        if not self.shown:  # the old pipeline printed an empty line for nothing
            write(b"\n")


def write(data):
    sys.stdout.buffer.write(data)
    sys.stdout.flush()


def summary(log, listening):
    """The tail every capture ends with, read from the whole log. The exit
    status: 1 for a panic, or for no output at all when the unit was reset."""
    with open(log, "rb") as f:
        plain = COLOUR.sub(b"", f.read()).rstrip(b"\n")
    lines = plain.split(b"\n")
    app = sum(1 for line in lines if APP.match(line))

    write(f"\n--- {app} application lines, full log: {log}\n".encode())

    # A panic is easy to miss among the noise, so call it out.
    if PANIC.search(plain):
        out, last = [], -1
        for i, line in enumerate(lines):
            if not PANIC_CONTEXT.search(line):
                continue
            if 0 <= last < i - 1:
                out.append(b"--")
            out.extend(lines[max(i, last + 1):i + 7])
            last = max(last, min(i + 6, len(lines) - 1))
        write(b"--- PANIC detected:\n" + b"".join(line + b"\n" for line in out[:12]))
        return 1

    if not app and listening:
        write(b"--- the unit printed nothing while listening; it logs little when idle.\n"
              b"    ./dev/capture --reset shows a whole boot (and restarts the unit).\n")
        return 0
    if not app:
        sys.stderr.write(NO_OUTPUT)
        return 1
    return 0


def run(command, log, lines, seconds, prog):
    """Run command, keep what it prints in log, show each line as it arrives.
    Ctrl-C, a second Ctrl-C, or the deadline stop it; what it prints while
    stopping is kept too."""
    try:
        proc = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    except OSError as e:
        _args.die(f"cannot run {command[0]}: {e.strerror}", prog)
    stopping = []  # when each stop was asked for

    def stop(*_):
        stopping.append(time.monotonic())
        try:
            proc.terminate() if len(stopping) == 1 else proc.kill()
        except ProcessLookupError:
            pass

    # Ctrl-C reaches the command too, being in the same process group; this
    # side only has to keep reading until the command has gone.
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    deadline = time.monotonic() + seconds if seconds else None
    fd = proc.stdout.fileno()
    pending = b""
    with open(log, "ab", buffering=0) as keep:
        while True:
            now = time.monotonic()
            if not stopping and deadline is not None and now >= deadline:
                stop()
            elif len(stopping) == 1 and now - stopping[0] > 3:
                stop()  # it ignored the first: kill it
            ready, _, _ = select.select([fd], [], [], 0.2)
            if not ready:
                continue
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            keep.write(chunk)
            *complete, pending = (pending + chunk).split(b"\n")
            for line in complete:
                lines.line(line)
    if pending:
        lines.line(pending)
    proc.wait()


def main():
    argv = sys.argv[1:]
    command = []
    if "--" in argv:
        cut = argv.index("--")
        argv, command = argv[:cut], argv[cut + 1:]
    ap = _args.Parser(__doc__)
    ap.add_argument("--as", dest="prog", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--filter")
    ap.add_argument("--listening", action="store_true")
    ap.add_argument("--seconds", type=float)
    args = ap.parse_args(argv)
    ap.prog = args.prog
    if (args.seconds is None) != (not command):
        ap.die("--seconds and a command go together. Try --help.")
    if args.seconds is not None and args.seconds < 0:
        ap.die(f"--seconds is 0 or more, not {args.seconds:g}")

    if args.filter is None:
        keep = APP.match
    else:
        keep = compile_filter(args.filter, args.prog).search
    lines = Lines(keep)

    if command:
        run(command, args.log, lines, args.seconds, args.prog)
    else:
        with open(args.log, "rb") as f:
            for line in COLOUR.sub(b"", f.read()).rstrip(b"\n").split(b"\n"):
                lines.line(line)
    lines.end()
    return summary(args.log, args.listening)


if __name__ == "__main__":
    sys.exit(main())
