# Troubleshooting the flash and monitor loop

- [No application output at all](#no-application-output-at-all)
- [Port problems](#port-problems)
- [Flashing fails](#flashing-fails)
- [The board resets](#the-board-resets)
- [Link errors](#link-errors)
- [Reading a panic](#reading-a-panic)

## No application output at all

The single most common failure on this hardware, and it looks like a hang.

Symptom: the bootloader banner and the `I (nnn) boot:` lines appear, the log
stops after `Disabling RNG early entropy source...`, and no `INFO - ` line ever
follows.

Cause: `esp-println` picks its output peripheral with the `auto` feature by
default, and `auto` selects the wrong one on this board. Its own documentation
names the case:

> Note: some boards, such as Waveshare's ESP32-C6-DEV-KIT-N8, come with a USB
> hub. This can confuse the `auto` feature. If you experience no log output, try
> selecting an output interface instead of relying on `auto`.

Fix: select the interface explicitly in `Cargo.toml`. On the dev kit that is
`uart`, whose output reaches the WCH bridge port.

```toml
esp-println = { version = "0.17.0", default-features = false, features = [
  "colors",
  "critical-section",
  "esp32c6",
  "log-04",
  "uart",
] }
```

`default-features = false` is required: `auto`, `colors` and `critical-section`
are the defaults, and `auto` and `uart` are mutually exclusive, so `auto` has to
be dropped rather than overridden. Dropping the defaults also removes `colors`
and `critical-section`, which is why both are listed again.

Verified: with `auto` the console showed no application line; after this change
the same binary printed `INFO - Embassy initialized!` followed by `Hello world!`
once a second.

On the ESP32-C6-Zero the answer is `jtag-serial` instead, because that board has
no bridge chip and its USB-C port is the chip's native USB.

**Which of the two you get is chosen by the board feature**, so a binary built
for the wrong board produces this exact symptom on a perfectly good one. A
dev-kit build on a Zero compiles, flashes and runs, and prints into a UART
nobody is listening to.

`./dev/flash.sh` builds the dev kit unless told otherwise, so on a Zero:

```sh
./dev/flash.sh --board zero --port /dev/cu.usbmodemXXXX
```

Both halves matter and they fail the same way. The boards enumerate as different
ports — the dev kit through its WCH bridge, the Zero as the chip's own USB — and
the default in `.cargo/config.toml` is the dev kit's. **The first `INFO` line
names the board it was built for** (`board: zero, id=99177c`), which settles
this in one glance; silence means look here first.

Second possible cause, if the interface is already explicit: the log level.
`ESP_LOG` is set to `info` in `.cargo/config.toml` and is read by
`esp-println`'s build script, so it is compiled into the binary by
`esp_println::logger::init_logger_from_env()`. Changing it requires
`cargo build`, not just a reflash. Messages below the compiled level are gone
from the image entirely.

Third: `esp_println::logger::init_logger_from_env()` must actually be called,
and called before anything that logs.

## Clicks that work briefly and then stop

Symptom: after a reset a few clicks arrive, then nothing ever again, while the
rest of the firmware carries on publishing happily. No panic, no jam beyond the
expected ones, just silence from the switch.

That is an **intermittent connection**, not a software fault. It cost most of a
session to diagnose, because it looks exactly like a stuck task and invites a
hunt through the async code.

What sends you down the wrong path: the boot line still reports the level
correctly, because a plain pin read works whether or not the button is on the
other end of the wire. A floating input with a pull-up simply reads `released`
forever.

The twenty-second test that settles it, with no tooling:

1. Hold the button down.
2. Tap RESET on the board.
3. Read the first `switch:` line.

`pressed` means the circuit is good and the problem is real. `released` while
you are holding it means the circuit is open and no firmware change will help.
Re-seat the jumper into the GPIO and the switch's legs, especially if anything
on the breadboard was touched since it last worked.

Two other clues that point at wiring rather than code:

- A minimal single-purpose binary sees the edges but the full firmware does not,
  and swapping the implementation changes nothing.
- The behaviour changes across resets with no rebuild in between.

## Port problems

`espflash list-ports` shows only one port on the dev kit. The board exposes two
and the hidden one is the useful one:

```sh
espflash list-ports --list-all-ports
```

| Port | Identified as | Behaviour |
|---|---|---|
| `/dev/cu.usbmodem11401` | Espressif USB JTAG/serial debug unit, 303a:1001 | `board-info` works; `monitor` fails with "Error while connecting to device", including with `--before usb-reset` |
| `/dev/cu.usbmodem5AAF2846061` | WCH USB-serial bridge, vid 1a86 | flash and monitor both work |

Use the bridge port on the dev kit and pin it so nothing has to guess:

```sh
export ESPFLASH_PORT=/dev/cu.usbmodem5AAF2846061
```

`--port` on any of the dev scripts overrides it for one run.

Both ports address the same chip, which `espflash board-info` confirms by
reporting the same MAC on each.

"Failed to connect to the device" on a port that worked a moment ago usually
means another monitor still holds it. Only one process can own a serial port.

## Flashing fails

- **Permission denied**: another `espflash`, a `screen`, or an editor's serial
  plugin has the port open.
- **Timed out waiting for packet header**: the chip is not entering the
  bootloader. Hold BOOT, tap RESET, release BOOT, then retry.
- **Wrong chip detected**: pass `--chip esp32c6` explicitly. The `cargo run`
  runner already does.
- **Binary too large**: the partition table gives the factory app 0x7f0000 of
  the 8 MB flash, so this is a symptom of a debug build bloating rather than a
  real limit. `[profile.dev]` already sets `opt-level = "s"` for that reason.

## Flash writes that seem not to stick

Two separate traps, both of which look like "the write silently failed".

### The board rewrote it between your two commands

`espflash erase-region` and `write-bin` **hard-reset the chip when they finish**
(`--after hard-reset` is the default). The board then boots whatever firmware is
already on it — which is not necessarily the firmware you are about to flash.

This cost real time once. The sequence was:

```sh
espflash erase-region 0x9000 0x1000     # record gone...
./dev/flash.sh                          # ...and back again
```

The erase worked. The reset then booted the *old* firmware, which still had
`seed_config`, and it wrote `cfg.toml` into flash within 350 ms — long before
the new binary was built, let alone flashed. The new firmware booted and found
a perfectly valid record.

The giveaway is that the record is *valid and current*, not corrupt. A failed
write leaves `store: no record yet`; a rewrite leaves
`store: configured for ...` with values you recognise.

**So: flash the new firmware first, then erase.** Whatever is running is what
gets a chance to write. `seed_config` is gone, so nothing in the current
firmware rewrites flash at boot — but anything added later that does will
recreate this exactly.

### `write-bin` does not erase

NOR flash can only clear bits, so writing a record over an existing one ANDs the
two together. `FDR2` written over `FDR1` becomes `FDR0`, and the firmware
reports `store: no record yet` — which reads as the write having failed rather
than as corruption.

`dev/provision.sh` erases the sector first, which is why that step is in the
script and must not be tidied away as redundant.

## The board resets

Read the `rst:` line in the boot banner. A healthy power-on shows:

```
rst:0x1 (POWERON),boot:0x1c (SPI_FAST_FLASH_BOOT)
```

Any other reset reason, especially one appearing the moment the motor starts, points at the supply rather than the firmware: the 220 µF capacitor across 5 V and ground next to the DRV8833 is missing, undersized, or too far from the driver. The motor's inrush drops the rail below the brown-out threshold.

A reset that repeats in a loop with the same reason a fixed interval apart is a
watchdog, not a brown-out. Some task is blocking the executor instead of
awaiting.

## Link errors

`build.rs` intercepts undefined symbols and prints a hint before the raw error.
Read the hint first:

| Undefined symbol | Meaning |
|---|---|
| starts with `esp_rtos_` | `esp-rtos` was never started, or `esp-radio` has no scheduler |
| `_stack_start` | `linkall.x` is missing from the linker scripts |
| `malloc`, `free`, `calloc` and friends | `esp-alloc` is missing, or its `compat` feature is off |
| starts with `_defmt_` | something pulled in `defmt`, which this project does not use |

## Reading a panic

`esp-backtrace` prints the panic and a backtrace over the same serial output.
The addresses are only useful with the matching ELF, so decode against the
binary that is actually flashed:

```sh
espflash monitor --non-interactive --port "$ESPFLASH_PORT" \
  --elf target/riscv32imac-unknown-none-elf/debug/cat-feeder
```

Passing `--elf` lets espflash symbolise the backtrace inline. Backtraces depend
on `-C force-frame-pointers` in `.cargo/config.toml`; removing that flag turns
every panic into a list of bare addresses.
