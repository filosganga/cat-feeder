# Headless is a build flag, not a probe

Superseded by [ADR-0027](0027-no-knob-and-no-panel.md): there is no other build.

`--features headless` leaves out the knob, menu and panel; GPIO3–GPIO5 are not
read at all. It is a flag rather than detected at boot because a broken panel
must not silently change what the knob's click does — and an unread pin cannot
trigger the power-on reset gesture through a stray bridge. A headless unit is
configured over its setup network, admin page and Home Assistant; its setup
password is only on the console, so the printed label is required.
