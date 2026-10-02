# Architecture decisions

One file per decision that is hard to reverse, surprising without context, and the result of a real trade-off. To change one, write a new ADR that supersedes it rather than editing history into the old one.

- [The unit owns its clock and its schedule](0001-the-unit-owns-its-clock-and-schedule.md)
- [A missed meal is preferable to a double one](0002-never-double-feed.md)
- [A retained time is not a trusted time](0003-a-retained-time-is-not-a-trusted-time.md)
- [Wi-Fi and broker credentials come from flash and nowhere else](0004-credentials-come-from-flash-only.md)
- [No automatic fall back to setup mode; resets forget only the network](0005-no-automatic-fallback-to-setup-mode.md)
- [Mechanical timing is one measured number per unit, stored in flash](0006-calibration-is-one-measured-number-per-unit.md)
- [Portions are the contract; clicks are the mechanism](0007-portions-are-the-contract-clicks-the-mechanism.md)
- [Feeding counts falling edges, and spacing is filtered in the feeder](0008-count-edges-and-filter-in-the-feeder.md)
- [One task owns the motor; decisions live in a pure state machine](0009-one-task-owns-the-motor.md)
- [The knob is the outside control, and a hold opens the menu](0010-the-knob-is-the-outside-control.md)
- [The status LED is dark when healthy, counts faults, and is solid for the mechanism](0011-the-led-is-dark-when-healthy.md)
- [The electronics live in their own printed case](0012-electronics-live-in-their-own-case.md)
- [The feeder's AA batteries are a backup through a diode-OR, never charged](0013-batteries-are-a-diode-or-backup.md)
- [Pause is broker state, and Home Assistant is its authority](0014-home-assistant-is-the-authority-for-pause.md) — superseded by 0023
- [Making feeders share meals is a copy in Home Assistant, not a broadcast topic](0015-syncing-schedules-is-a-copy-in-home-assistant.md)
- [The unit keeps summer time from a POSIX rule its admin page derives](0016-the-unit-keeps-summer-time-from-a-browser-derived-rule.md)
- [The admin page authenticates with the unit's derived password](0017-the-admin-page-uses-the-derived-password.md)
- [A meal is a position in the schedule](0018-a-meal-is-a-position.md)
- [The broker is configured as an IPv4 address](0019-the-broker-is-an-ipv4-address.md)
- [No sound](0020-no-sound.md)
- [Headless is a build flag, not a probe](0021-headless-is-a-build-flag.md)
- [OTA is two app slots, pushed to the admin page, confirmed by the broker](0022-ota-is-two-slots-pushed-to-the-admin-page.md)
- [The unit is the authority for pause](0023-the-unit-is-the-authority-for-pause.md)
- [The unit boots our own bootloader, and it is what rolls back](0024-our-own-bootloader-rolls-back.md)
