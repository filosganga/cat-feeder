# The netlist is written down, and the drawing is checked against it

`dev/netlist.toml` says what connects to what on the perfboard, net by net.
`pcb.diy` only says where the parts sit and how the copper runs, and
`dev/pcb-check` fails on any difference between the two: a net split, two
nets joined, a pin wired that no net has. A net that reaches a GPIO names the
`src/board.rs` macro that drives it, and the check holds both to the same pin.

Until now the drawing was the only statement of the circuit. The check derived
the nets from it and judged them by local rules (pin order, supplies, spare
pins), so a drawing that agreed with itself passed even when it connected the
wrong things: a data line run to the wrong resistor broke no rule. The intent
lived in three places that nothing compared: `board.rs`'s pin table, the
tables in `docs/hardware.md`, and the `unused` lists in `dev/pinouts.toml`.
Those lists move into the netlist, where a pin in no net is spare;
`pinouts.toml` is left with the parts alone.

This is what circuit-board tools call layout versus schematic. KiCad would
give the same check and more (a schematic with ERC, DRC, body outlines and 3D
fit), at the cost of redrawing the board and leaving a perfboard-shaped tool.
It is on the roadmap; the TOML file is the part of it that pays now, with no
new dependency.

What follows. A wiring change starts in the netlist and then in the drawing.
Anything made from the netlist, such as the KiCad netlist and module
footprints `dev/kicad-netlist` writes for other tools, is generated and
never edited; a change made there is lost on the next export.
The netlist cannot be checked against anything but the firmware's pins: a net
it should not contain is a mistake only a reader catches.
