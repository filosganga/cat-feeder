"""The perfboard drawing as the dev tools read it: one parser, shared by
dev/pcb-check, dev/pcb-render and dev/kicad-netlist, because two parsers would
be two opinions about the board. Not meant to be run.

Why each rule is shaped the way it is:

- A straight copper trace touches every hole it passes over, not just its two
  ends, because on a perfboard each of those holes gets soldered. A trace that
  is not straight or 45 degrees runs between holes and joins only its ends.
- Jumpers and wires join their two ends and nothing between: they are insulated
  and on the component side.
- The netlist is only as good as the pin names in the drawing, which is exactly
  what went wrong once, hence the pinout check against the part.
- A drawing can be self-consistent and still connect the wrong things: every
  rule above passes a data line run to the wrong resistor. Only a statement of
  what is meant, made apart from the drawing, catches that (ADR-0030).

Needs Python 3.11 for tomllib; a tool checks that before importing this.
"""

import re
import tomllib
import xml.etree.ElementTree as ET

from _args import die as fail  # a broken input is reported as the tool's own error

# Pin names that are a supply rail, and what kind. Lower-case, as compared.
GROUND = {"gnd"}
RAIL_5V = {"5v", "vcc", "vbatt", "vin", "vm"}
RAIL_3V3 = {"3v3", "3.3v"}

SEVERITY = {"short": 0, "high": 1, "medium": 2, "low": 3}


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
    symmetric = set()  # two-lead parts that work either way round, by name

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
            # A resistor fitted the other way round is the same circuit; a diode
            # is not. Electrolytics were handled above, by polarity.
            if el.tag.startswith("diylc.passive."):
                symmetric.add(name)
            continue
        if kind == "TactileMicroSwitch":
            for i, g in enumerate(grid[:4]):
                pin(g, f"{name}.{i + 1}")
            continue
        notes.append(f"{name} is a {kind}, which this check does not understand; its pins are not in the netlist")

    return board, nets, pins, headers, labels, notes, symmetric


def load_netlist(path):
    """hardware/netlist.toml as {net name: (pins, firmware macro or None)}, or exits.

    Mistakes in the file itself are reported here rather than as drawing faults:
    a pin in two nets, or a net of one pin, is a netlist that says nothing true.
    """
    try:
        with open(path, "rb") as f:
            data = tomllib.load(f)
    except OSError as e:
        fail(f"cannot read {path}: {e}")
    except tomllib.TOMLDecodeError as e:
        fail(f"{path} is not TOML: {e}")
    nets = {}
    seen = {}
    for name, body in data.get("net", {}).items():
        extra = set(body) - {"pins", "firmware"}
        if extra:
            fail(f"{path}: net {name} has {', '.join(sorted(extra))}; a net has only pins and firmware")
        pins_ = list(body.get("pins", []))
        if len(pins_) < 2:
            fail(f"{path}: net {name} has {len(pins_)} pin(s); a net joins at least two")
        for p in pins_:
            if p.lower() in seen:
                fail(f"{path}: {p} is in both {seen[p.lower()]} and {name}")
            seen[p.lower()] = name
        nets[name] = (pins_, body.get("firmware"))
    if not nets:
        fail(f"{path} has no [net.<name>] tables")
    return nets


def compare(drawn, netlist, symmetric):
    """The drawing's nets against the netlist: layout versus schematic.

    `drawn` is a list of pin-name lists, one per net of the drawing, singletons
    included. Returns (problems, names): what disagrees, in words, and for each
    drawn net (by index) the netlist name it carries when exactly one does.

    A pin in no net of the netlist is meant to be connected to nothing, so the
    netlist alone says which pins are spare.
    """
    drawn = [list(n) for n in drawn]
    where = {}  # lower-case pin -> index of its drawn net
    for i, n in enumerate(drawn):
        for p in n:
            where[p.lower()] = i
    meant = {}  # lower-case pin -> netlist name
    for name, (pins_, _) in netlist.items():
        for p in pins_:
            meant[p.lower()] = name

    # A resistor fitted either way round is right, so for each one take the
    # orientation that agrees best with the netlist before comparing.
    for part in sorted(symmetric):
        a, b = f"{part}.1".lower(), f"{part}.2".lower()
        if a not in where or b not in where or a not in meant or b not in meant:
            continue

        def agree(lead, net_name):
            others = {p.lower() for p in netlist[net_name][0]} - {a, b}
            return len(others & {p.lower() for p in drawn[where[lead]]})

        if agree(b, meant[a]) + agree(a, meant[b]) > agree(a, meant[a]) + agree(b, meant[b]):
            ia, ib = where[a], where[b]
            drawn[ia] = [f"{part}.2" if p.lower() == a else p for p in drawn[ia]]
            drawn[ib] = [f"{part}.1" if p.lower() == b else p for p in drawn[ib]]
            where[a], where[b] = ib, ia

    def group(i):
        n = drawn[i]
        return f"{{{' '.join(n)}}}" + (" (connected to nothing)" if len(n) == 1 else "")

    problems = []
    for name, (pins_, _) in netlist.items():
        absent = [p for p in pins_ if p.lower() not in where]
        for p in absent:
            problems.append(f"{name}: {p} is not in the drawing")
        parts = sorted({where[p.lower()] for p in pins_ if p.lower() in where})
        if len(parts) > 1:
            problems.append(f"{name} is split: " + " | ".join(group(i) for i in parts))

    names = {}
    for i, n in enumerate(drawn):
        here = sorted({meant[p.lower()] for p in n if p.lower() in meant})
        stray = [p for p in n if p.lower() not in meant]
        if len(here) > 1:
            problems.append(f"{' and '.join(here)} are joined: {' '.join(n)}")
        if stray and len(n) > 1:
            into = f"net {here[0]}" if len(here) == 1 else " ".join(p for p in n if p not in stray) or "each other"
            problems.append(f"{' '.join(stray)} reach{'es' if len(stray) == 1 else ''} {into}, "
                            "but no net in the netlist has them")
        if len(here) == 1:
            names[i] = here[0]
    return problems, names


def board_pins(path):
    """{macro name: GPIO number} from src/board.rs's `$peripherals.GPIOn` macros."""
    try:
        text = open(path).read()
    except OSError as e:
        fail(f"cannot read {path}: {e}")
    found = dict(
        (m.group(1), int(m.group(2)))
        for m in re.finditer(r"macro_rules!\s+(\w+)\s*\{.*?\$peripherals\.GPIO(\d+)", text, re.S)
    )
    if not found:
        fail(f"{path} has no `$peripherals.GPIOn` pin macros; has its shape changed?")
    return found


def firmware(netlist, macros):
    """Each net's GPIO against the pin the firmware hands out for it.

    A net names its macro (`firmware = "switch_pin"`); the GPn pin in it must be
    the GPIO that macro returns. Every macro must be claimed by a net, and every
    net reaching a GPn pin must name one, so a pin the firmware drives and a pin
    the board wires cannot drift apart unseen.
    """
    problems = []
    claimed = set()
    for name, (pins_, macro) in netlist.items():
        gpios = sorted({int(m.group(1)) for p in pins_ if (m := re.fullmatch(r"gp(\d+)", p.rsplit(".", 1)[-1].lower()))})
        if macro is None:
            if gpios:
                problems.append(f"{name} reaches GPIO{gpios[0]} but names no board.rs macro (firmware = \"...\")")
            continue
        claimed.add(macro)
        if macro not in macros:
            problems.append(f"{name} names {macro}, which board.rs does not define")
        elif gpios != [macros[macro]]:
            have = ", ".join(f"GPIO{g}" for g in gpios) or "no GPn pin"
            problems.append(f"{name}: board.rs gives {macro} GPIO{macros[macro]}, the netlist has {have}")
    for macro, gpio in sorted(macros.items()):
        if macro not in claimed:
            problems.append(f"board.rs gives {macro} GPIO{gpio}, and no net says where it goes")
    return problems


def classify(names):
    low = {n.rsplit(".", 1)[-1].lower() for n in names}
    if low & GROUND:
        return "GND"
    if low & RAIL_3V3:
        return "3V3"
    if low & RAIL_5V or any(n.endswith(".+") for n in names):
        return "5V"
    return None
