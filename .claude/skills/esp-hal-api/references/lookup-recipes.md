# Lookup recipes

- [Resolve the version](#resolve-the-version)
- [Grep the vendored source](#grep-the-vendored-source)
- [Build the docs locally](#build-the-docs-locally)
- [Read repo examples with gh](#read-repo-examples-with-gh)
- [Worked example](#worked-example-the-microswitch-input)
- [Sources](#sources)

## Resolve the version

```sh
cargo tree -p esp-hal --depth 0     # esp-hal v1.1.2
cargo tree -p esp-radio --depth 0
cargo tree -p esp-rtos --depth 0
cargo tree -p embassy-time --depth 0
```

`cargo tree` reports the resolved version from `Cargo.lock`. Note that
`Cargo.toml` may pin `~1.1.0` while the lock resolves `1.1.2`; patch releases of
esp-hal do add and deprecate items, so always use the lock's number.

## Grep the vendored source

```sh
R=$(ls -d ~/.cargo/registry/src/index.crates.io-*/esp-hal-1.1.2)

rg -n "pub fn|pub async fn" "$R/src/gpio/mod.rs"       # constructors, setters
rg -n "pub struct InputConfig" -A 20 "$R/src/gpio/mod.rs"
rg -l "Ledc|rmt" "$R/src"                               # find the module first
```

Useful module map for this project:

| Need | File under `src/` |
|---|---|
| GPIO in/out, pull, drive | `gpio/mod.rs` |
| `wait_for_falling_edge` and friends | `gpio/asynch.rs` |
| Clocks, `CpuClock` | `clock/mod.rs` |
| Timer groups | `timer/timg.rs` |
| `Instant`, `Duration` | `time.rs` |
| RMT (addressable RGB LED) | `rmt.rs` |
| Software interrupts | `interrupt/software.rs` |

The same trick works for every dependency: swap `esp-hal-1.1.2` for
`esp-radio-0.18.0`, `esp-rtos-0.3.0`, `embassy-time-0.5.1`, and so on.

The vendored directories also carry `CHANGELOG.md` and `MIGRATING-*.md` for
some crates (`esp-radio` has `MIGRATING-0.16.0.md` and `MIGRATING-0.17.0.md`).
Those files are the fastest way to fix a snippet written for an older release.

## Build the docs locally

Renders the pinned version with this project's exact feature set, offline:

```sh
cargo doc -p esp-hal --no-deps
# -> target/riscv32imac-unknown-none-elf/doc/esp_hal/index.html
```

Add `--open` when a human is driving. An agent should prefer grepping the
source, which is cheaper and quotable.

## Read repo examples with gh

Examples live in `esp-rs/esp-hal` at the tag for the pinned version. Confirm the
tag exists first — tags are per-crate, e.g. `esp-hal-v1.1.2`, `esp-radio-v0.18.0`,
`esp-rtos-v0.3.0`:

```sh
gh api repos/esp-rs/esp-hal/tags --paginate --jq '.[].name' | grep '^esp-hal-v'
```

List the example groups, then an example's files:

```sh
gh api "repos/esp-rs/esp-hal/contents/examples?ref=esp-hal-v1.1.2" --jq '.[].name'
gh api "repos/esp-rs/esp-hal/contents/examples/wifi/embassy_dhcp/src?ref=esp-hal-v1.1.2" --jq '.[].name'
```

Fetch a file's contents:

```sh
gh api "repos/esp-rs/esp-hal/contents/examples/wifi/embassy_dhcp/src/main.rs?ref=esp-hal-v1.1.2" \
  --jq '.content' | base64 -d
```

Quote the URL in `gh api` — an unquoted `?` is a glob in zsh and the call fails
with "no matches found".

Groups at `esp-hal-v1.1.2`: `async`, `ble`, `esp-now`, `hello_world`,
`ieee802154`, `interrupt`, `ota`, `peripheral`, `wifi`. Each example is a
standalone crate with its own `Cargo.toml`, so its feature list also shows which
features an API needs.

Two more directories in the same repo are good API references:

- `qa-test/src/bin/` — small single-file programs per peripheral, including
  `gpio_interrupt_latency.rs` and several `embassy_wifi_*.rs`.
- `hil-test/` — hardware-in-the-loop tests, the most precise usage examples.

## Worked example: the microswitch input

Claim to verify: `Input::new(pin, InputConfig::default().with_pull(Pull::Up))`.

```sh
R=$(ls -d ~/.cargo/registry/src/index.crates.io-*/esp-hal-1.1.2)
rg -n "pub fn new" "$R/src/gpio/mod.rs"
```

```
1090:    pub fn new(pin: impl InputPin + 'd, config: InputConfig) -> Self {
```

Confirmed for 1.1.2, at `esp-hal-1.1.2/src/gpio/mod.rs:1090`. The awaitable edge
helpers are separate:

```sh
rg -n "pub async fn wait_for" "$R/src/gpio/asynch.rs"
```

```
80:    pub async fn wait_for_falling_edge(&mut self) {
90:    pub async fn wait_for_any_edge(&mut self) {
```

That is enough to write the debounced click stream without guessing.

## Sources

- esp-hal on docs.rs, version-pinned: <https://docs.rs/esp-hal/1.1.2/esp_hal/>
- esp-radio on docs.rs: <https://docs.rs/esp-radio/0.18.0/esp_radio/>
- esp-rtos on docs.rs: <https://docs.rs/esp-rtos/0.3.0/esp_rtos/>
- Repository and examples: <https://github.com/esp-rs/esp-hal>
