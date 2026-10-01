#!/usr/bin/env python3
"""Checks a DIY Layout Creator perfboard drawing. Run it through pcb-check.sh.

Reads the layout's XML and reports, in order:

  pinouts     each header's pin names against dev/pinouts.toml, the parts' own
              silkscreen order
  power       nets that join ground to a supply, 5 V to 3V3, or hold a polarised
              capacitor the wrong way round
  nets        every net with its pins, and the pins connected to nothing
  labels      printed labels that disagree with the pin they sit on
  neighbours  holes a pitch apart on different nets, worst first — where a solder
              blob does damage

Exits 1 if anything in pinouts, power or labels is wrong, 0 otherwise. Neighbours
and unconnected pins are for reading, not failing: a perfboard always has them.

Why each rule is shaped the way it is:

- A straight copper trace touches every hole it passes over, not just its two
  ends, because on a perfboard each of those holes gets soldered. A trace that
  is not straight or 45 degrees runs between holes and joins only its ends.
- Jumpers and wires join their two ends and nothing between: they are insulated
  and on the component side.
- The netlist is only as good as the pin names in the drawing, which is exactly
  what went wrong once, hence the pinout check against the part.
"""

import argparse
import sys
import tomllib
import xml.etree.ElementTree as ET
from collections import defaultdict
from itertools import combinations

# Pin names that are a supply rail, and what kind. Lower-case, as compared.
GROUND = {"gnd"}
RAIL_5V = {"5v", "vcc", "vbatt", "vin", "vm"}
RAIL_3V3 = {"3v3", "3.3v"}

SEVERITY = {"short": 0, "high": 1, "medium": 2, "low": 3}


def fail(msg):
    print(f"pcb-check: {msg}", file=sys.stderr)
    sys.exit(2)


class Board:
    """The hole grid: turns DIYLC's inch coordinates into printed hole names."""

    def __init__(self, el):
        pts = [p for p in el.iter("point")]
        (x1, y1), (x2, y2) = [(float(p.get("x")), float(p.get("y"))) for p in pts[:2]]
        self.x1, self.y1 = min(x1, x2), min(y1, y2)
        sp = el.find("spacing")
        self.pitch = float(sp.get("value")) if sp is not None else 0.1
        if sp is not None and sp.get("unit") == "mm":
            self.pitch /= 25.4
        self.cols = round(abs(x2 - x1) / self.pitch) - 1
        self.rows = round(abs(y2 - y1) / self.pitch) - 1
        # Rows count from the bottom on a board set to Bottom_Left, which is
        # how the real board's numbers read; anything else counts from the top.
        self.bottom = (el.findtext("coordinateOrigin") or "Bottom_Left").startswith("Bottom")

    def grid(self, p):
        """(column, row) indices, or None for a point off the hole grid."""
        cx = (float(p.get("x")) - self.x1) / self.pitch - 1
        cy = (float(p.get("y")) - self.y1) / self.pitch - 1
        c, r = round(cx), round(cy)
        if abs(cx - c) > 0.05 or abs(cy - r) > 0.05:
            return None
        return (c, r)

    def name(self, g):
        c, r = g
        letters = ""
        n = c
        while True:
            letters = chr(ord("A") + n % 26) + letters
            n = n // 26 - 1
            if n < 0:
                break
        row = self.rows - r if self.bottom else r + 1
        return f"{letters}{row}"

    def on_board(self, g):
        return 0 <= g[0] < self.cols and 0 <= g[1] < self.rows


class Nets:
    """Union-find over holes."""

    def __init__(self):
        self.parent = {}

    def find(self, a):
        self.parent.setdefault(a, a)
        while self.parent[a] != a:
            self.parent[a] = self.parent[self.parent[a]]
            a = self.parent[a]
        return a

    def join(self, a, b):
        self.parent[self.find(a)] = self.find(b)


def straight_path(a, b):
    """Every hole a straight or 45-degree trace covers, ends included."""
    dx, dy = b[0] - a[0], b[1] - a[1]
    steps = max(abs(dx), abs(dy))
    if steps == 0:
        return [a]
    if dx and dy and abs(dx) != abs(dy):
        return None
    sx = (dx > 0) - (dx < 0)
    sy = (dy > 0) - (dy < 0)
    return [(a[0] + i * sx, a[1] + i * sy) for i in range(steps + 1)]


def control_points(el):
    holder = el.find("controlPoints")
    if holder is None:
        holder = el.find("points")
    return list(holder.iter("point")) if holder is not None else []


def load(path):
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as e:
        fail(f"cannot read {path}: {e}")
    components = root.find("components")
    boards = [c for c in components if c.tag.endswith("PerfBoard")]
    if len(boards) != 1:
        fail(f"{path} has {len(boards)} perfboards; this checks exactly one")
    board = Board(boards[0])

    nets = Nets()
    pins = {}  # hole -> "Part.pin"
    headers = {}  # header name -> [pin names in drawing order]
    labels = []  # (hole or None, text)
    notes = []  # things the reader should know the check could not do

    def pin(g, label):
        if g is None:
            notes.append(f"{label} is off the hole grid; left out")
            return
        if g in pins:
            notes.append(f"{board.name(g)} holds both {pins[g]} and {label}")
        pins[g] = label
        nets.find(g)

    for el in components:
        kind = el.tag.rsplit(".", 1)[-1]
        name = el.findtext("name") or kind
        pts = control_points(el)
        grid = [board.grid(p) for p in pts]

        if kind == "PerfBoard" or kind == "Rectangle" or kind.startswith("Ellipse"):
            continue
        if kind == "Label":
            p = el.find("point")
            labels.append((board.grid(p) if p is not None else None, el.findtext("text") or ""))
            continue
        if kind == "CopperTrace":
            a, b = grid[0], grid[1]
            if a is None or b is None:
                notes.append(f"trace {name} ends off the hole grid; left out")
                continue
            path = straight_path(a, b) or [a, b]
            for g in path:
                nets.join(g, a)
            continue
        if kind == "Jumper" or "Wire" in kind:
            ends = [g for g in (grid[:2] if kind == "Jumper" else [grid[0], grid[-1]])]
            if None in ends:
                notes.append(f"{name} ends off the hole grid; left out")
                continue
            nets.join(ends[0], ends[1])
            continue
        if kind == "PinHeader":
            names = (el.findtext("nodeNames") or "").split(",")
            if len(names) != len(pts):
                notes.append(
                    f"header {name} has {len(pts)} pins but {len(names)} names; "
                    "unnamed pins are numbered"
                )
                names = [names[i] if i < len(names) and names[i] else str(i + 1) for i in range(len(pts))]
            headers[name] = names
            for g, n in zip(grid, names):
                pin(g, f"{name}.{n}")
            continue
        if kind == "RadialElectrolytic" or kind == "AxialElectrolyticCapacitor":
            # The first lead is + unless the part is drawn inverted. Polarity is
            # what the power check reads, so it is named rather than numbered.
            plus, minus = ("-", "+") if (el.findtext("invert") == "true") else ("+", "-")
            pin(grid[0], f"{name}.{plus}")
            pin(grid[1], f"{name}.{minus}")
            continue
        if kind.startswith("Transistor") or kind.startswith("TO") or kind.startswith("Regulator"):
            for i, g in enumerate(grid[:3]):
                pin(g, f"{name}.{i + 1}")
            continue
        if el.tag.startswith("diylc.passive.") or kind.startswith("Diode") or kind.startswith("LED"):
            # Two-lead parts carry a third, label-placement point after the leads.
            for i, g in enumerate(grid[:2]):
                pin(g, f"{name}.{i + 1}")
            continue
        if kind == "TactileMicroSwitch":
            for i, g in enumerate(grid[:4]):
                pin(g, f"{name}.{i + 1}")
            continue
        notes.append(f"{name} is a {kind}, which this check does not understand; its pins are not in the netlist")

    return board, nets, pins, headers, labels, notes


def classify(names):
    low = {n.rsplit(".", 1)[-1].lower() for n in names}
    if low & GROUND:
        return "GND"
    if low & RAIL_3V3:
        return "3V3"
    if low & RAIL_5V or any(n.endswith(".+") for n in names):
        return "5V"
    return None


def main():
    ap = argparse.ArgumentParser(add_help=False)
    ap.add_argument("--layout", default="pcb.diy")
    ap.add_argument("--pinouts", default="dev/pinouts.toml")
    ap.add_argument("--all", action="store_true")
    args = ap.parse_args()

    board, nets, pins, headers, labels, notes = load(args.layout)
    try:
        with open(args.pinouts, "rb") as f:
            reference = tomllib.load(f)
    except OSError as e:
        fail(f"cannot read {args.pinouts}: {e}")
    except tomllib.TOMLDecodeError as e:
        fail(f"{args.pinouts} is not TOML: {e}")

    errors = 0

    # Group holes into nets; a net's name is its pins.
    members = defaultdict(list)
    for g in list(nets.parent):
        members[nets.find(g)].append(g)
    net_pins = {root: sorted(pins[g] for g in holes if g in pins) for root, holes in members.items()}

    print(f"{args.layout}: {board.cols} x {board.rows} holes, {len(headers)} headers, {len(pins)} pins\n")

    # --- pinouts ---------------------------------------------------------
    print("Pinouts, against the parts' silkscreen")
    directions = defaultdict(set)
    for name, drawn in headers.items():
        ref = reference.get(name)
        if ref is None:
            print(f"  ??    {name}: no entry in {args.pinouts}, so unchecked")
            continue
        want = [p.lower() for p in ref["pins"]]
        got = [p.lower() for p in drawn]
        module = ref.get("module", name)
        if got == want:
            directions[module].add("forward")
            print(f"  ok    {name}")
        elif got == want[::-1]:
            directions[module].add("reversed")
            print(f"  ok    {name} (reversed)")
        else:
            errors += 1
            print(f"  FAIL  {name}: drawn   {' '.join(drawn)}")
            print(f"        {' ' * len(name)}  the part {' '.join(ref['pins'])}")
    for module, ways in directions.items():
        if len(ways) > 1:
            errors += 1
            print(f"  FAIL  {module}: some rows match forwards and some backwards, which no placement of one part does")
    for name in reference:
        if name not in headers:
            print(f"  --    {name} is in {args.pinouts} but not in the drawing")

    # --- power -----------------------------------------------------------
    print("\nPower")
    power_ok = True
    for root, names in net_pins.items():
        low = {n.rsplit(".", 1)[-1].lower() for n in names}
        if low & GROUND and (low & (RAIL_5V | RAIL_3V3) or any(n.endswith(".+") for n in names)):
            errors += 1
            power_ok = False
            print(f"  FAIL  ground joined to a supply: {' '.join(names)}")
        elif low & RAIL_5V - {"vcc"} and low & RAIL_3V3:
            errors += 1
            power_ok = False
            print(f"  FAIL  5 V joined to 3V3: {' '.join(names)}")
        minus = [n for n in names if n.endswith(".-")]
        if minus and not low & GROUND and classify(names) is not None:
            errors += 1
            power_ok = False
            print(f"  FAIL  a capacitor's - lead on a supply: {' '.join(names)}")
    if power_ok:
        print("  ok    no supply joined to ground or to another supply")

    # --- nets ------------------------------------------------------------
    print("\nNets")
    connected = sorted((n for n in net_pins.values() if len(n) > 1), key=lambda n: (classify(n) is None, n))
    for names in connected:
        kind = classify(names) or ""
        print(f"  {kind:4}  {' '.join(names)}")
    alone = sorted(n[0] for n in net_pins.values() if len(n) == 1)
    print(f"\n  connected to nothing: {' '.join(alone) if alone else 'none'}")

    # --- unused ----------------------------------------------------------
    # A pin left unconnected on purpose is listed as `unused` in the pinouts
    # file; any other pin connected to nothing is a missing trace or jumper.
    # This is what catches a signal that never arrives — a switch line that
    # stops one hole short of the jumper that should carry it on.
    print("\nUnconnected pins")
    unused_ok = True
    for full in alone:
        header, _, pin_name = full.rpartition(".")
        ref = reference.get(header)
        if ref is None:
            continue
        intended = {p.lower() for p in ref.get("unused", [])}
        if pin_name.lower() not in intended:
            errors += 1
            unused_ok = False
            print(f"  FAIL  {full} is connected to nothing, and is not listed as unused")
    alone_set = set(alone)
    for header, ref in reference.items():
        for pin_name in ref.get("unused", []):
            match = [p for p in alone_set if p.lower() == f"{header}.{pin_name}".lower()]
            if header in headers and not match:
                print(f"  --    {header}.{pin_name} is listed as unused but is connected")
    if unused_ok:
        print("  ok    every unconnected pin is meant to be")

    # --- labels ----------------------------------------------------------
    print("\nLabels")
    labels_ok = True
    for g, text in labels:
        if g is None or not board.on_board(g):
            print(f"  --    '{text}' is off the board")
            continue
        at = pins.get(g)
        if at is None:
            continue
        pin_name = at.rsplit(".", 1)[-1]
        if pin_name.lower() != text.lower() and not (
            {pin_name.lower(), text.lower()} <= RAIL_5V
        ):
            errors += 1
            labels_ok = False
            print(f"  FAIL  {board.name(g)} is labelled '{text}' but the pin is {at}")
    if labels_ok:
        print("  ok    every label on a pin names that pin")

    # --- neighbours ------------------------------------------------------
    def describe(root):
        # A kind alone is ambiguous: USB, the battery and the rail are all 5 V
        # nets, and a bridge between two of them is its own fault.
        names = net_pins[root]
        if not names:
            return "unnamed"
        kind = classify(names)
        return f"{kind} {names[0]}" if kind else names[0]

    def severity(a, b):
        ca, cb = classify(net_pins[a]), classify(net_pins[b])
        kinds = {ca, cb}
        if "GND" in kinds and kinds & {"5V", "3V3"}:
            return "short"
        if "5V" in kinds:
            return "high"
        if "3V3" in kinds:
            return "medium"
        return "low"

    pairs = defaultdict(list)
    for g in nets.parent:
        for d in ((1, 0), (0, 1)):
            h = (g[0] + d[0], g[1] + d[1])
            if h in nets.parent and nets.find(h) != nets.find(g):
                a, b = sorted((nets.find(g), nets.find(h)), key=describe)
                pairs[(a, b)].append(f"{board.name(g)}|{board.name(h)}")

    print("\nNeighbours a hole apart on different nets, worst first")
    ranked = sorted(pairs.items(), key=lambda kv: (SEVERITY[severity(*kv[0])], -len(kv[1])))
    hidden = 0
    for (a, b), holes in ranked:
        level = severity(a, b)
        if level == "low" and not args.all:
            hidden += 1
            continue
        shown = " ".join(sorted(holes)[:8]) + (" ..." if len(holes) > 8 else "")
        print(f"  {level:6}  {describe(a)} | {describe(b)}  x{len(holes)}  {shown}")
    if hidden:
        print(f"  ({hidden} signal-to-signal pairs not shown; --all lists them)")

    if notes:
        print("\nNot checked")
        for n in notes:
            print(f"  --    {n}")

    print(f"\n{'FAILED' if errors else 'ok'}: {errors} problem{'s' if errors != 1 else ''}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
