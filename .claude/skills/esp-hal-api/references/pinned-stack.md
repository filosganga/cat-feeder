# Traps in this project's pinned stack

- [Feature gates](#feature-gates)
- [The executor is esp-rtos](#the-executor-is-esp-rtos)
- [Wi-Fi lives in esp-radio](#wi-fi-lives-in-esp-radio)
- [Build configuration you must not break](#build-configuration-you-must-not-break)
- [Signals that a snippet is from the wrong era](#signals-that-a-snippet-is-from-the-wrong-era)

## Feature gates

`esp-hal` splits its API into a stable core and a large `unstable` surface. This
project enables `unstable`, so those items are available, but they are marked
with an instability attribute in the source and are missing from any docs build
that omits the feature. If an item exists in the vendored source but the
compiler says it is private or missing, check the gate before assuming a version
mismatch.

Each esp crate also needs its chip feature (`esp32c6`) and, for logging through
the `log` crate, `log-04`. When adding a dependency that talks to esp-hal,
mirror those three.

docs.rs builds `esp-hal` with `["esp32c6", "unstable"]` on the
`riscv32imac-unknown-none-elf` target, which matches this project. That is why
the docs.rs pages can be trusted here without extra feature juggling.

## The executor is esp-rtos

This project does **not** use `esp-hal-embassy`. Most tutorials and older
examples do, so their entry point and init code do not apply.

What is actually in `src/bin/main.rs`, verified against the vendored crates:

- `#[esp_rtos::main]` on `async fn main(spawner: Spawner) -> !` — `esp-rtos`
  re-exports this as `pub use macros::rtos_main as main`.
- `esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0)` starts the
  scheduler, after `TimerGroup::new(peripherals.TIMG0)` and
  `SoftwareInterruptControl::new(peripherals.SW_INTERRUPT)`.
- Spawned tasks still use `#[embassy_executor::task]`.

`esp-rtos` documents one hard constraint: do **not** enable any `arch-*` feature
on `embassy-executor`. `esp-rtos` supplies the architecture layer; enabling both
produces duplicate-symbol or missing-scheduler link errors. In this project
`embassy-executor` carries only the `log` feature, and it must stay that way.

A related link error is caught by `build.rs`: an undefined symbol starting with
`esp_rtos_` prints a hint that the scheduler was never initialised.

## Wi-Fi lives in esp-radio

Wi-Fi is `esp_radio::wifi`, initialised in `main` as:

```rust
let (mut _wifi_controller, _interfaces) =
    esp_radio::wifi::new(peripherals.WIFI, Default::default())
        .expect("Failed to initialize Wi-Fi controller");
```

`esp-radio` is the successor of `esp-wifi`; anything referring to `esp_wifi::`
is for an older stack and needs translating, not copying. The vendored
`esp-radio-0.18.0/` directory contains `CHANGELOG.md`, `MIGRATING-0.16.0.md` and
`MIGRATING-0.17.0.md` — read them when porting.

The network stack is `embassy-net` over `smoltcp`, with the feature set already
chosen in `Cargo.toml`. Adding a protocol usually means adding a `smoltcp`
feature too.

## Build configuration you must not break

`.cargo/config.toml` carries settings the build depends on:

- `target = "riscv32imac-unknown-none-elf"` and
  `build-std = ["alloc", "core"]`, so `cargo build` needs no `--target`.
- `runner = "espflash flash --monitor --chip esp32c6"`, which is what makes
  `cargo run` flash the board.
- `rustflags = ["-C", "force-frame-pointers"]`, required for `esp-backtrace`
  backtraces.
- `ESP_LOG="info"`, read at **build time** by `esp-println`'s build script and
  baked into the binary by `esp_println::logger::init_logger_from_env()`.
  Changing the log level therefore requires a rebuild, not just a restart.

`esp-alloc` is initialised with `esp_alloc::heap_allocator!` in `main` because
`esp-radio` needs a heap. Keep allocation out of the rest of the firmware.

## Signals that a snippet is from the wrong era

Treat these as proof the snippet predates the pinned versions, and look the API
up again rather than adapting it:

| In the snippet | Why it is stale |
|---|---|
| `esp_wifi::` | renamed to `esp-radio` |
| `esp_hal_embassy::init(...)` | this project uses `esp-rtos` |
| `#[esp_hal_embassy::main]` or `#[embassy_executor::main]` | entry point is `#[esp_rtos::main]` |
| `Input::new(pin, Pull::Up)` (two args, no config struct) | 1.x takes an `InputConfig` |
| `peripherals.GPIO8.into_push_pull_output()` | pre-1.0 typestate GPIO API |
| `esp_hal::peripherals::Peripherals::take()` | replaced by `esp_hal::init(config)` |
| `embassy-executor` with an `arch-riscv32` feature | conflicts with `esp-rtos` |
