#!/usr/bin/env python3
"""Tests for dev/_render.py:  python3 dev/_render_test.py

The capture command is a stand-in: a Python one-liner printing log lines with
pauses between them, so these run anywhere, with no unit attached.
"""

import os
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

RENDER = Path(__file__).resolve().with_name("_render.py")

BOOT = [
    "ESP-ROM:esp32c6-20220919",
    "\x1b[0;32mINFO (326) - board: zero, id=a1b2c3\x1b[0m",
    "INFO (397) - wifi: connecting to MyWiFi",
    "INFO (2884) - wifi: associated",
    "WARN (4912) - mqtt: broker refused",
]


def unit(lines, then="", pause=0.3):
    """A command printing lines a pause apart, then running `then`."""
    body = "".join(f"print({line!r}, flush=True); time.sleep({pause}); " for line in lines)
    return [sys.executable, "-c", f"import time, signal; {body}{then}"]


class Live(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.log = os.path.join(self.dir.name, "capture.log")

    def tearDown(self):
        self.dir.cleanup()

    def start(self, command, seconds, *flags):
        return subprocess.Popen(
            [sys.executable, str(RENDER), "--as", "thing", "--log", self.log, "--seconds", str(seconds), *flags, "--", *command],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)

    def finished(self, *flags):
        r = subprocess.run([sys.executable, str(RENDER), "--as", "thing", "--log", self.log, *flags], capture_output=True)
        return r.returncode, r.stdout

    def test_lines_show_as_they_arrive_not_at_the_end(self):
        p = self.start(unit(BOOT, then="time.sleep(30)"), 0)
        ready, _, _ = select.select([p.stdout], [], [], 5)
        first = p.stdout.readline() if ready else b""
        self.assertTrue(first.startswith(b"INFO (326) - board: zero"), first)
        self.assertIsNone(p.poll(), "the command was still running when the line came")
        os.killpg(p.pid, signal.SIGINT)
        p.communicate(timeout=10)

    def test_what_it_shows_live_is_what_the_finished_log_renders_to(self):
        for flags in ((), ("--filter", "wifi:|ESP-ROM"), ("--listening",)):
            if os.path.exists(self.log):
                os.remove(self.log)
            p = self.start(unit(BOOT, pause=0.05), 30, *flags)
            live, _ = p.communicate(timeout=20)
            self.assertEqual((p.returncode, live), self.finished(*flags), flags)

    def test_ctrl_c_ends_it_with_the_summary_and_keeps_the_log(self):
        p = self.start(unit(BOOT[:3], then="time.sleep(30)", pause=0.05), 0)
        time.sleep(1.5)
        os.killpg(p.pid, signal.SIGINT)  # as the terminal does: every process in the group
        out, err = p.communicate(timeout=10)
        self.assertEqual(p.returncode, 0, err)
        self.assertIn(b"INFO (397) - wifi: connecting to MyWiFi", out)
        self.assertIn(f"--- 2 application lines, full log: {self.log}\n".encode(), out)
        self.assertIn(b"wifi: connecting", Path(self.log).read_bytes())
        self.assertNotIn(b"Traceback", out + err)

    def test_the_deadline_ends_it_with_the_summary(self):
        start = time.monotonic()
        p = self.start(unit(BOOT[:2], then="time.sleep(30)", pause=0.05), 1)
        out, _ = p.communicate(timeout=10)
        self.assertLess(time.monotonic() - start, 5)
        self.assertIn(b"--- 1 application lines", out)

    def test_a_command_that_ignores_being_stopped_is_killed(self):
        stubborn = "signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)"
        start = time.monotonic()
        p = self.start(unit(BOOT[:2], then=stubborn, pause=0.05), 1)
        out, _ = p.communicate(timeout=15)
        self.assertLess(time.monotonic() - start, 8)
        self.assertIn(b"--- 1 application lines", out)

    def test_a_panic_is_called_out_and_fails(self):
        p = self.start(unit(BOOT[:2] + ["ERROR (500) - panicked at src/feeder.rs:10:5"], pause=0.05), 30)
        out, _ = p.communicate(timeout=20)
        self.assertEqual(p.returncode, 1)
        self.assertIn(b"--- PANIC detected:\nERROR (500) - panicked at src/feeder.rs:10:5\n", out)

    def test_silence_while_listening_is_a_quiet_unit(self):
        p = self.start(unit(["ESP-ROM:esp32c6-20220919"], pause=0.05), 30, "--listening")
        out, _ = p.communicate(timeout=20)
        self.assertEqual(p.returncode, 0)
        self.assertIn(b"the unit printed nothing while listening", out)


class Finished(unittest.TestCase):
    def render(self, text, *flags):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(text)
        try:
            r = subprocess.run([sys.executable, str(RENDER), "--as", "thing", "--log", f.name, *flags], capture_output=True)
        finally:
            os.unlink(f.name)
        return r.returncode, r.stdout.split(b"\n--- ")[0]

    def test_each_line_carries_the_gap_on_the_unit_s_clock_padded_by_bytes(self):
        code, out = self.render(b"INFO (100) - a\nINFO (350) - caf\xc3\xa9\n")
        self.assertEqual(code, 0)
        self.assertEqual(out, b"INFO (100) - a\n" + b"INFO (350) - caf\xc3\xa9".ljust(72) + b" (+250 ms)\n")

    def test_a_command_that_cannot_run_is_an_error_not_a_traceback(self):
        r = subprocess.run([sys.executable, str(RENDER), "--as", "thing", "--log", os.devnull, "--seconds", "1",
                            "--", "no-such-command-anywhere"], capture_output=True)
        self.assertEqual((r.returncode, r.stderr), (2, b"thing: cannot run no-such-command-anywhere: No such file or directory\n"))

    def test_a_posix_class_it_cannot_translate_is_refused(self):
        r = subprocess.run([sys.executable, str(RENDER), "--as", "thing", "--log", os.devnull,
                            "--filter", "[[:print:]]+b"], capture_output=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn(b"[:print:] is not a class this can read", r.stderr)

    def test_a_filter_that_is_not_a_regular_expression_is_refused(self):
        r = subprocess.run([sys.executable, str(RENDER), "--as", "thing", "--log", os.devnull, "--filter", "("], capture_output=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn(b"thing: --filter '(' is not a regular expression", r.stderr)


if __name__ == "__main__":
    unittest.main()
