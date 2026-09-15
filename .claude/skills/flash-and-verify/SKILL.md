---
name: flash-and-verify
description: Runs the build, flash and serial-monitor loop for this ESP32-C6 firmware and says what the serial console must show for a change to count as working, step by step along the roadmap. Use after changing firmware, when asked whether a change actually works on hardware, when the monitor shows nothing or the wrong port is picked, or when deciding whether a roadmap step is done.
---

# Flash and verify

`cargo build` proves the code compiles. Only the serial console proves it runs.
There is no debugger in this project, so a change is unverified until a specific
log line appears.

## The loop

```sh
cargo build                 # fast, catches the common case
cargo run                   # build + espflash flash --monitor --chip esp32c6
```

`cargo run` works without extra flags because `.cargo/config.toml` sets both the
target and the `espflash` runner. Exit the monitor with Ctrl+C.

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

An agent must not start an interactive monitor, which never returns.

```sh
timeout 20 espflash monitor --non-interactive --port "$ESPFLASH_PORT"
timeout 90 espflash flash --monitor --non-interactive --chip esp32c6 \
  --port "$ESPFLASH_PORT" target/riscv32imac-unknown-none-elf/debug/cat-feeder
```

`--non-interactive` skips the port picker and reset prompts. `timeout` bounds
the capture, since the monitor otherwise runs forever.

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
INFO (364) - switch: waiting for clicks on GPIO11, currently released
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

Only step 1 has been observed on hardware. Every later step describes what the
console *must* show, and the person who first reaches that step should correct
the file with what it actually showed.

## Rules

- Change one thing, flash, read the console, then change the next. A batch of
  changes that produces silence tells you nothing.
- Never report a step as working from a successful build alone. Quote the log
  line you saw.
- `ESP_LOG` in `.cargo/config.toml` is read at **build time**. Changing the log
  level needs a rebuild, not just a reflash.
- If the console contradicts the code, trust the console.
