#!/usr/bin/env python3
"""Reads a serial port, without resetting what is on the other end, until it
is stopped. Used by dev/capture, through dev/_render.py, which stops it at the
deadline or at Ctrl-C; not meant to be called directly.

    _tap.py <port>

espflash's monitor resets the chip when it attaches, even told not to: it
talks to the ROM to identify the chip. This only opens the port and reads.
HUPCL is cleared so closing it does not drop the modem lines either.
Standard library only, so nothing to install.
"""

import os
import select
import signal
import sys
import termios
import time

port = sys.argv[1]

# Stopped is the normal way to end, not an error: no traceback in the log.
# Everything read is already written, a chunk at a time.
signal.signal(signal.SIGINT, signal.SIG_DFL)
signal.signal(signal.SIGTERM, signal.SIG_DFL)


def open_port():
    fd = os.open(port, os.O_RDONLY | os.O_NOCTTY | os.O_NONBLOCK)
    attrs = termios.tcgetattr(fd)
    attrs[0] = 0  # iflag: raw
    attrs[1] = 0  # oflag: raw
    attrs[2] = (attrs[2] | termios.CREAD | termios.CLOCAL) & ~termios.HUPCL
    attrs[3] = 0  # lflag: no echo, not canonical
    termios.tcsetattr(fd, termios.TCSANOW, attrs)
    return fd


# The Zero's console is the chip's own USB, which disappears whenever the chip
# resets on its own (an OTA restart, the watchdog). Rather than end the
# capture there, wait for the port to come back and keep reading: the reboot
# that follows is usually what was worth seeing.
fd = None
while True:
    if fd is None:
        try:
            fd = open_port()
        except OSError:
            time.sleep(0.2)
            continue
    try:
        ready, _, _ = select.select([fd], [], [], 0.5)
        if not ready:
            continue
        data = os.read(fd, 4096)
    except BlockingIOError:
        continue
    except OSError:
        os.close(fd)
        fd = None
        sys.stdout.buffer.write(b"\n--- port gone (the unit reset); waiting for it\n")
        sys.stdout.flush()
        continue
    if data:
        sys.stdout.buffer.write(data)
        sys.stdout.flush()
    else:
        # EOF: the device went away without an error.
        os.close(fd)
        fd = None
