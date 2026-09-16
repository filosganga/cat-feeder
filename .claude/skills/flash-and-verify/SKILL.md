---
name: flash-and-verify
description: Runs the build, flash and serial-monitor loop for this ESP32-C6 firmware and says what the serial console must show for a change to count as working, step by step along the roadmap. Use after changing firmware, when asked whether a change actually works on hardware, when the monitor shows nothing or the wrong port is picked, or when deciding whether a roadmap step is done.
---

# Flash and verify

`cargo build` proves the code compiles. Only the serial console proves it runs.
There is no debugger in this project, so a change is unverified until a specific
log line appears.

## The loop

**Use the scripts.** They encode the port, the flags and the log rendering, and
they fail loudly on the mistakes that otherwise cost a whole run.

```sh
./dev/flash.sh                        # build, flash, capture 45 s
./dev/flash.sh 90                     # ...capture 90 s instead
./dev/capture.sh 60 'feed:|switch:'   # capture without reflashing, filtered
```

Both print every line annotated with the milliseconds since the previous one,
which is what makes a feed cycle readable, and both keep the unfiltered log and
print its path. They exit non-zero on a panic or on no application output at
all, with an explanation of the likely cause.

For an interactive session, `cargo run` also works and needs no flags, because
`.cargo/config.toml` sets the target, the `espflash` runner and `ESPFLASH_PORT`.
Exit the monitor with Ctrl+C.

Do **not** hand-roll the espflash invocation. Two traps have cost real time:
`--no-reset` loads a flash stub that halts the application so only the
bootloader prints, and the default port is the wrong one of the two this board
exposes. The scripts avoid both.

## Pick the port, every time

This board presents **two** serial ports and espflash picks the wrong one by
default.

```sh
espflash list-ports --list-all-ports
```

Observed on the ESP32-C6-DEV-KIT-N8:

| Port | Device | Use |
|---|---|---|
| `/dev/cu.usbmodem11401` | Espressif USB JTAG/serial debug unit, 303a:1001 | `board-info` works, `monitor` fails to connect |
| `/dev/cu.usbmodem5AAF2846061` | WCH USB-serial bridge, vid 1a86 | flashing and monitoring both work |

Plain `espflash list-ports` hides the bridge port, because it only lists devices
it recognises as development boards. Always pass `--list-all-ports`.

Set the port once so `cargo run` never prompts:

```sh
export ESPFLASH_PORT=/dev/cu.usbmodem5AAF2846061
```

The serial number in that path is per-cable and per-board. Re-read it from
`list-ports` after plugging in a different unit.

Confirm the board and cable before blaming the firmware:

```sh
espflash board-info
```

It prints the chip revision, 8MB flash size, `WiFi 6, BT 5` and the MAC address.
That MAC is the source of the `<id>` used in every MQTT topic.

## Running it unattended

An agent must not start an interactive monitor, which never returns. The
scripts are already bounded and non-interactive, so they are safe to call
directly:

```sh
./dev/flash.sh 60
./dev/capture.sh 30 'mqtt:'
```

Captures that need a button pressed at the right moment cannot be automated.
Start the capture in the background, tell the user what to press, and read the
result when it finishes. Everything else, including boot sequences, MQTT
behaviour and the jam timeout, runs with nobody at the bench.

## What a healthy boot looks like

Verified on the dev kit. The ESP-IDF second-stage bootloader speaks first, then
the application:

```
ESP-ROM:esp32c6-20220919
rst:0x1 (POWERON),boot:0x1c (SPI_FAST_FLASH_BOOT)
I (23) boot: ESP-IDF v6.1-beta1-497-g14f663f003e 2nd stage bootloader
I (63) boot:  2 factory          factory app      00 00 00010000 007f0000
I (198) boot: Loaded app from partition at offset 0x10000
I (198) boot: Disabling RNG early entropy source...
INFO (261) - Embassy initialized!
INFO (264) - board: devkit, id=db0260
INFO (358) - wifi: connecting to <ssid>
INFO (364) - switch: watching GPIO2, currently released
INFO (1621) - wifi: associated
INFO (11643) - wifi: connected, ip=192.168.68.123/24
INFO (11797) - mqtt: connected, id=feeder_<id>
INFO (11821) - mqtt: online
```

Two different timestamp formats share this log and they are not the same clock.
`I (nnn) boot:` lines come from the ESP-IDF bootloader and appear even when the
application is dead. **Only the lines formatted `LEVEL (ms) - ...` are yours**,
stamped with milliseconds since boot.

If the log stops at "Disabling RNG early entropy source", the application
produced no output — see
[references/troubleshooting.md](references/troubleshooting.md), starting with
the `esp-println` output interface.

Sample logs elsewhere in this skill omit the millisecond field for readability.
Real output always carries it.

## Per-step expectations

[references/serial-expectations.md](references/serial-expectations.md) lists,
for each roadmap step, the log lines that step must emit and the physical action
that triggers them. Treat it as the acceptance test: implement the lines it
names, then flash and read them back.

Most steps carry real transcripts now. The ones still unobserved are 3 (the
motor itself), 6 (flashing the Zeros), 8, the form half of 9, and the
visual half of 10 — every one of them waiting on hardware, a phone, or someone
looking at the board. Those describe what the console *must* show, and whoever
first reaches one should correct the file with what it actually showed.

## Rules

- Change one thing, flash, read the console, then change the next. A batch of
  changes that produces silence tells you nothing.
- Never report a step as working from a successful build alone. Quote the log
  line you saw.
- `ESP_LOG` in `.cargo/config.toml` is read at **build time**. Changing the log
  level needs a rebuild, not just a reflash.
- **Flash new firmware before erasing flash regions, never after.** Both
  `espflash erase-region` and `write-bin` hard-reset the chip, so the board
  boots whatever is currently on it and can rewrite what you just erased. This
  has already cost one debugging session; see
  [references/troubleshooting.md](references/troubleshooting.md).
- If the console contradicts the code, trust the console.
