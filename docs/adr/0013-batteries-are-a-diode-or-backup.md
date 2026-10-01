# The feeder's AA batteries are a backup through a diode-OR, never charged

USB 5 V and the feeder's own 3×AA reach the rail through a dual common-cathode
Schottky (MBRF2045CT): whichever is higher feeds the board, and neither can push
current into the other. A plain wire from the battery pin to the rail would
charge alkaline cells from USB.

Estimated, not measured: a C6 on Wi-Fi draws 80–120 mA, so three AAs are
roughly a day — a bridge across a power cut, not a way to run. The firmware
does not know which supply it is on; there is no supply detection and no sleep
mode.

## Consequences

The rail sits a Schottky drop below USB, so a laptop on the board's own USB
(through its onboard diode) and the adapter share the rail at about the same
voltage. Unplug the laptop before provoking a jam.
