# The status LED is dark when healthy, counts faults, and is solid for the mechanism

The LED is the only output that survives the network being the broken thing.

- **Dark is healthy.** If lit were normal, nobody would look at it. A double
  green flash marks entering health, so a boot loop reads as repeating green.
- **Faults are counted, not coloured**: one, two, three red flashes for no
  Wi-Fi, no broker, no trusted time — three different fixes, readable across a
  dark room and by a colour-blind reader.
- **Solid means the mechanism, blinking means the network**, which keeps a jam
  unambiguous.
- A jam outranks everything, including the armed menu, because someone may have
  their hands in the hub; the panel carries the "menu is open" confirmation
  instead. An armed menu outranks the network faults.

The power-on red/green/blue sweep stays: with dark meaning healthy it is the
only moment a dead LED is distinguishable. It also proved these boards'
WS2812s take **RGB** order, not the datasheet's GRB.
