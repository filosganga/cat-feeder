#!/usr/bin/env python3
"""Tests for dev/_args.py:  python3 dev/_args_test.py

The bash half is tested the way a script uses it: the spec and the command
line go in, and what comes out is evaluated by bash. The specs under "today's
scripts" are the ones the bash scripts in dev/ pass, and their cases are the
command lines each accepted or refused when it still had its own parse loop.
"""

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import _args  # noqa: E402

ARGS_PY = Path(__file__).resolve().with_name("_args.py")

HEADER = """#!/usr/bin/env bash
# Does a thing.
#
#   ./dev/thing --port p

set -euo pipefail
"""


def python_parser():
    ap = _args.Parser("Does a thing.\n\n  ./dev/thing", prog="thing")
    ap.add_argument("--layout", default="pcb.diy")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--side", choices=["component", "solder"], default="component")
    return ap


def run_python(argv):
    """(exit code, stdout, stderr, namespace or None)."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            ns = python_parser().parse_args(argv)
            code = 0
        except SystemExit as e:
            ns, code = None, e.code
    return code, out.getvalue(), err.getvalue(), ns


class PythonTools(unittest.TestCase):
    def test_an_option_not_given_takes_its_default(self):
        code, _, _, ns = run_python([])
        self.assertEqual((code, ns.layout, ns.all, ns.side), (0, "pcb.diy", False, "component"))

    def test_a_value_follows_its_option_as_a_word_or_after_an_equals_sign(self):
        self.assertEqual(run_python(["--layout", "a.diy"])[3].layout, "a.diy")
        self.assertEqual(run_python(["--layout=a.diy"])[3].layout, "a.diy")

    def test_a_value_starting_with_a_dash_is_still_a_value(self):
        self.assertEqual(run_python(["--layout", "-odd.diy"])[3].layout, "-odd.diy")

    def test_help_prints_the_docstring_and_exits_0(self):
        code, out, _, _ = run_python(["--help"])
        self.assertEqual((code, out), (0, "Does a thing.\n\n  ./dev/thing\n"))
        self.assertEqual(run_python(["-h"])[0], 0)

    def test_a_command_line_called_wrong_exits_2_naming_the_tool(self):
        cases = {
            ("--bogus",): "thing: unknown option '--bogus'. Try --help.\n",
            ("--bogus=1",): "thing: unknown option '--bogus=1'. Try --help.\n",
            ("stray",): "thing: unexpected argument 'stray'. Try --help.\n",
            ("--layout",): "thing: --layout needs a value\n",
            ("--layout=",): "thing: --layout needs a value\n",
            ("--lay", "x"): "thing: unknown option '--lay'. Try --help.\n",
            ("--all=1",): "thing: --all takes no value. Try --help.\n",
        }
        for argv, message in cases.items():
            code, out, err, _ = run_python(list(argv))
            self.assertEqual((code, out, err), (2, "", message), argv)

    def test_a_value_outside_the_choices_exits_2(self):
        code, _, err, _ = run_python(["--side", "top"])
        self.assertEqual(code, 2)
        self.assertTrue(err.startswith("thing: --side: invalid choice: 'top'"), err)

    def test_alternatives_are_given_at_most_once_between_them(self):
        def parser():
            ap = _args.Parser("x", prog="thing")
            ap.alternatives(["--password", "--pass"], ["--password-file"])
            return ap

        self.assertEqual(parser().parse_args(["--pass", "p"]).password, "p")
        for argv, message in (
            (["--password", "a", "--pass", "b"], "thing: --password given twice.\n"),
            (["--password", "a", "--password-file", "-"],
             "thing: --password and --password-file are alternatives; pass one.\n"),
        ):
            err = io.StringIO()
            with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as e:
                parser().parse_args(argv)
            self.assertEqual((e.exception.code, err.getvalue()), (2, message))

    def test_too_old_a_python_is_refused_in_one_line(self):
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as e:
            _args.need_python(99, 0, "the future")
        self.assertEqual(e.exception.code, 2)
        self.assertIn(": needs Python 99.0 or later for the future; this is Python ", err.getvalue())


class Shell(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.script = Path(self.dir.name) / "thing"
        self.script.write_text(HEADER)

    def tearDown(self):
        self.dir.cleanup()

    def parse(self, spec, argv, stdin=None):
        """Evaluate the result in bash, as a script does, and report every
        variable the spec names, NUL-separated so any value comes back whole.
        (exit code, {var: value or [values]}, stdout, stderr)"""
        names = sorted({item.rpartition("=")[2] for flag, item in zip(spec[::2], spec[1::2])
                        if flag not in ("--refuse", "--alternatives")})
        report = "".join(
            f'if declare -p {n} 2>/dev/null | grep -q "^declare -a"; then for v in "${{{n}[@]}}"; do '
            f'printf "%s\\0%s\\0" "{n}[]" "$v" >>"$REPORT"; done; '
            f'elif [ -n "${{{n}+x}}" ]; then printf "%s\\0%s\\0" "{n}" "${{{n}}}" >>"$REPORT"; fi\n'
            for n in names
        )
        bash = (
            f'ARGS=$(python3 "{ARGS_PY}" "{self.script}" "$@") || exit $?\n'
            'eval "$ARGS"\n' + report + 'printf "%s\\0%s\\0" stdin "$(cat)" >>"$REPORT"\n'
        )
        report_file = Path(self.dir.name) / "report"
        report_file.write_bytes(b"")
        r = subprocess.run(["bash", "-c", bash, "bash", *spec, "--", *argv], input=stdin or "",
                           capture_output=True, text=True, env={**os.environ, "REPORT": str(report_file)})
        fields = report_file.read_bytes().decode().split("\0")[:-1]
        got, self.stdin_seen = {}, None
        for key, value in zip(fields[::2], fields[1::2]):
            if key == "stdin":
                self.stdin_seen = value
            elif key.endswith("[]"):
                got.setdefault(key[:-2], []).append(value)
            else:
                got[key] = value
        return r.returncode, got, r.stdout, r.stderr

    def test_any_value_survives_eval_whole_in_either_spelling(self):
        nasty = "it's a \"quoted\" $value `with` ; | & * \\ \n second line"
        for argv in (["--port", nasty], [f"--port={nasty}"]):
            self.assertEqual(self.parse(["--value", "port=PORT"], argv)[:2], (0, {"PORT": nasty}), argv)

    def test_stdin_is_left_for_the_script(self):
        self.parse(["--value", "password-file=FILE"], ["--password-file", "-"], stdin="secret")
        self.assertEqual(self.stdin_seen, "secret")

    def test_only_what_was_given_is_assigned(self):
        self.assertEqual(self.parse(["--value", "port=PORT", "--flag", "reset=RESET"], [])[:2], (0, {}))

    def test_help_prints_the_header_and_nothing_else_runs(self):
        code, _, out, err = self.parse(["--value", "port=PORT"], ["--help", "--bogus"])
        self.assertEqual((code, out, err), (0, "Does a thing.\n\n  ./dev/thing --port p\n", ""))
        self.assertIsNone(self.stdin_seen)

    def test_a_command_line_called_wrong_exits_2_naming_the_script(self):
        spec = ["--value", "port=PORT", "--positional", "SECONDS"]
        for argv, message in (
            (["--bogus"], "thing: unknown option '--bogus'. Try --help.\n"),
            (["1", "2"], "thing: unexpected argument '2'. Try --help.\n"),
            (["--port"], "thing: --port needs a value\n"),
            (["--port="], "thing: --port needs a value\n"),
        ):
            code, _, _, err = self.parse(spec, argv)
            self.assertEqual((code, err), (2, message), argv)
            self.assertIsNone(self.stdin_seen, argv)

    def test_a_bad_spec_is_the_script_s_fault(self):
        code, _, _, err = self.parse(["--value", "port"], [])
        self.assertEqual(code, 2)
        self.assertIn("_args.py: bad spec in", err)

    # --- today's scripts ------------------------------------------------------

    FLASH = ["--value", "board=BOARD_ARG", "--value", "port=PORT_ARG",
             "--value", "seconds=SECONDS_TO_CAPTURE", "--value", "filter=FILTER",
             "--positional", "SECONDS_TO_CAPTURE", "--positional", "FILTER"]

    def test_flash_keeps_its_positionals_in_command_line_order(self):
        cases = (
            (["90", "feed:"], {"SECONDS_TO_CAPTURE": "90", "FILTER": "feed:"}),
            (["--seconds", "5", "feed:"], {"SECONDS_TO_CAPTURE": "5", "FILTER": "feed:"}),
            (["90", "--seconds", "5"], {"SECONDS_TO_CAPTURE": "5"}),
            (["--board", "zero", "--filter", "-x"], {"BOARD_ARG": "zero", "FILTER": "-x"}),
        )
        for argv, want in cases:
            self.assertEqual(self.parse(self.FLASH, argv)[:2], (0, want), argv)
        self.assertEqual(self.parse(self.FLASH, ["1", "2", "3"])[0], 2)

    def test_a_word_like_a_negative_number_is_an_unknown_option(self):
        for spec, argv in (
            (["--value", "hours=HOURS", "--positional", "HOURS"], ["-5"]),
            (self.FLASH, ["90", "-2"]),
            (["--value", "port=PORT_ARG", "--list", "ids"], ["-1"]),
        ):
            code, _, _, err = self.parse(spec, argv)
            self.assertEqual((code, err), (2, f"thing: unknown option '{argv[-1]}'. Try --help.\n"), argv)

    def test_capture_takes_reset_as_a_flag(self):
        spec = self.FLASH[2:] + ["--flag", "reset=RESET"]
        self.assertEqual(self.parse(spec, ["--reset", "15"])[1], {"RESET": "1", "SECONDS_TO_CAPTURE": "15"})

    def test_ap_password_takes_any_number_of_ids_and_no_out(self):
        spec = ["--value", "port=PORT_ARG", "--list", "ids"]
        self.assertEqual(self.parse(spec, ["a1b2c3", "--port", "p", "d4e5f6"])[1],
                         {"PORT_ARG": "p", "ids": ["a1b2c3", "d4e5f6"]})
        self.assertEqual(self.parse(spec, [])[:2], (0, {}))
        self.assertEqual(self.parse(spec, ["--out", "x"])[0], 2)

    def test_label_takes_ids_and_an_out(self):
        spec = ["--value", "port=PORT_ARG", "--value", "out=OUT", "--list", "ids"]
        self.assertEqual(self.parse(spec, ["a1b2c3", "--out", "l.html"])[1], {"OUT": "l.html", "ids": ["a1b2c3"]})

    def test_watch_takes_topics_and_one_password(self):
        spec = ["--value", "host=HOST_ARG", "--value", "user=USER_ARG",
                "--value", "password,pass=PASS_ARG", "--value", "password-file=PASS_FILE_ARG",
                "--alternatives", "password,password-file", "--list", "TOPICS"]
        self.assertEqual(self.parse(spec, ["feeder/#", "--pass", "x", "--", "--odd"])[1],
                         {"PASS_ARG": "x", "TOPICS": ["feeder/#", "--odd"]})
        self.assertEqual(self.parse(spec, ["--", "-1"])[1], {"TOPICS": ["-1"]})
        code, _, _, err = self.parse(spec, ["--password", "a", "--password-file", "-"])
        self.assertEqual((code, err), (2, "thing: --password and --password-file are alternatives; pass one.\n"))
        code, _, _, err = self.parse(spec, ["--password", "a", "--pass", "b"])
        self.assertEqual((code, err), (2, "thing: --password given twice.\n"))

    PROVISION = ["--value", "port=PORT_ARG", "--value", "host=HOST_ARG", "--value", "user=USER_ARG",
                 "--value", "password,pass=PASS_ARG", "--value", "password-file=PASS_FILE_ARG",
                 "--value", "nvs-offset=OFFSET_ARG", "--alternatives", "password,password-file",
                 "--refuse", "out=--out is not yours to set; the record is a temporary file.",
                 "--forward", "MKRECORD_ARGS"]

    def test_provision_forwards_what_it_does_not_know_to_mkrecord(self):
        code, got, _, _ = self.parse(self.PROVISION, ["--detent-ms", "900", "--host", "auto",
                                                      "--portion-scale=133", "--password-file", "-"],
                                     stdin="pw")
        self.assertEqual((code, got), (0, {"HOST_ARG": "auto", "PASS_FILE_ARG": "-",
                                           "MKRECORD_ARGS": ["--detent-ms", "900", "--portion-scale=133"]}))
        self.assertEqual(self.stdin_seen, "pw")
        # An unknown option followed by another option is forwarded on its own.
        self.assertEqual(self.parse(self.PROVISION, ["--detent-ms", "--host", "auto"])[1],
                         {"HOST_ARG": "auto", "MKRECORD_ARGS": ["--detent-ms"]})

    def test_provision_refuses_out_however_it_is_spelled(self):
        for argv in (["--out", "x"], ["--out=x"], ["--out"]):
            code, _, _, err = self.parse(self.PROVISION, argv)
            self.assertEqual((code, err), (2, "thing: --out is not yours to set; the record is a temporary file.\n"))
        self.assertEqual(self.parse(self.PROVISION, ["stray"])[3], "thing: unexpected argument 'stray'. Try --help.\n")

    def test_soak_takes_hours_either_way_and_the_later_wins(self):
        spec = ["--value", "port=PORT_ARG", "--value", "hours=HOURS", "--positional", "HOURS"]
        self.assertEqual(self.parse(spec, ["3"])[1], {"HOURS": "3"})
        self.assertEqual(self.parse(spec, ["3", "--hours", "4"])[:2], (0, {"HOURS": "4"}))

    def test_soak_report_takes_a_log(self):
        self.assertEqual(self.parse(["--value", "log=LOG", "--positional", "LOG"], ["soak/x.log"])[1],
                         {"LOG": "soak/x.log"})

    def test_ota_and_bootloader_take_no_bare_arguments(self):
        ota = ["--value", "address=ADDRESS", "--value", "id=ID", "--value", "board=BOARD_ARG"]
        self.assertEqual(self.parse(ota, ["--address=1.2.3.4", "--id", "a1b2c3"])[1],
                         {"ADDRESS": "1.2.3.4", "ID": "a1b2c3"})
        self.assertEqual(self.parse(ota, ["1.2.3.4"])[0], 2)
        self.assertEqual(self.parse([], [])[0], 0)
        self.assertEqual(self.parse([], ["x"])[0], 2)


if __name__ == "__main__":
    unittest.main()
