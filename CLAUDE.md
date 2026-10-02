# cat-feeder — ESP32-C6 firmware (Rust, no_std, Embassy)

Replacement electronics for commercial automatic cat feeders. The original
board is removed; the mechanics are kept: a 5 V geared motor and a microswitch
on the output hub that clicks **once per portion**. Several units feed together,
coordinated by Home Assistant over MQTT, and each keeps feeding on its own when
the network is gone.

Where things live:

| | |
|---|---|
| this file | what you need before changing code: rules, contracts, gotchas, workflow |
| [docs/adr/](docs/adr/README.md) | *why* — one file per design decision |
| [docs/hardware.md](docs/hardware.md) | wiring, power, the perfboard, the beep test |
| [docs/roadmap.md](docs/roadmap.md) | open work |
| [README.md](README.md) | the builder's guide: parts, flashing, setup, using it |
| `CLAUDE.local.md` | git-ignored: this machine, its units, its Home Assistant |
| skills | procedures loaded on demand: `flash-and-verify`, `pcb-review`, `ha-mqtt-discovery`, `esp-hal-api`, `dev-script` |

## Keeping this file small

It is loaded into every session, so every line costs. **Budget: 400 lines,
checked in CI** (`.github/workflows/rust_ci.yml`). Before adding to it:

- **Only what is true now and needed to change code.** No dates, no "used to",
  no "superseded", no "found the hard way", no ✅/⬜ status. Git log keeps
  history; `docs/roadmap.md` keeps open work.
- **A decision is one line here plus an ADR** (`docs/adr/NNNN-slug.md`, a
  paragraph or two) when it is hard to reverse, surprising, and a real
  trade-off. Changing one means a new ADR, not an edited old one.
- **No verification transcripts.** Evidence goes in the commit message; what the
  console should show goes in the `flash-and-verify` skill.
- **Nothing about specific units, addresses or this house** — that is
  `CLAUDE.local.md`. The repo is public and generic.
- **Say it once.** Link to the code, an ADR or a doc rather than restating it.
- Hardware build detail goes in `docs/hardware.md`; procedures go in a skill.

## Hardware the code depends on

Boards: **ESP32-C6-Zero** in production (8 MB flash — the schematic's
`C6FH4` suggests 4 MB; `espflash board-info` is the authority), the
ESP32-C6-DEV-KIT-N8 for development. Same chip; Cargo features
`board-devkit` (default) / `board-zero`, plus `headless` (ADR-0021).
`--no-default-features` is required for the Zero: two board features at once
fail in esp-println's build script.

**`src/board.rs` is the only place a pin number lives**, with the assignment
table to solder against. Constraints behind it:

- The Zero does not bring out GPIO10/11. GPIO8, 9, 15 are strapping that
  matters (boot mode, boot log, JTAG); GPIO12/13 are native USB, the Zero's
  only console; GPIO8 is also the onboard WS2812. GPIO4/5 are strapping pins
  only for SDIO edges, so the encoder may use them.
- On the C6 peripherals route through the GPIO matrix, so any free pin will do.

| Part | What the firmware assumes |
|---|---|
| hub switch, GPIO2 | internal pull-up, other leg to ground: idle high, press = **falling** edge. An optical or hall sensor is a different shape entirely |
| DRV8833 | `IN1=1,IN2=0` forward, `0,0` coast, `1,1` brake; `nSLEEP` driven high or nothing turns. No reverse, deliberately (ADR-0009) |
| motor | geared; stops dead on brake. Detent interval is per model, ~1.9 s to ~5.3 s known (ADR-0006) |
| WS2812 on GPIO8 | **RGB** wire order, not the datasheet's GRB; `led::wire_word` is the one place to change |
| I²C, GPIO18 SDA / GPIO19 SCL | SSD1306-compatible panel at `0x3C`/`0x3D`, DS3231 at `0x68` (a PCF8563 would be `0x51`) |
| panel | 128×64, `FONT_6X10`: `display::ROWS` 6, **`display::COLS` 21**. `Line` is `String<COLS>` and `push` truncates silently — lay out every line against 21 |
| DS3231 | its `OSF` flag is the reason for the part (ADR-0001); alarm 2's registers hold the UTC offset (ADR-0016) |

Safety for bench work (detail in `docs/hardware.md`): don't hold a jam on USB
alone — the motor's stall current then runs through the Zero's 1 A Schottky;
on dev-kit header J1, ground and GPIO3 sit beside 5 V.

## Feeding

One mechanical contract: **1 click = 1 portion**, after
`portions::clicks_for` applies the unit's scale (ADR-0007).

- `feed(n)`: **align** (switch open → run to the first falling edge,
  uncounted), **count** n falling edges, **brake on the nth**. Starting level
  never matters (ADR-0008).
- `switch.rs` debounces at `feeder::DEBOUNCE_MS` (30) and reports every edge.
  `feeder.rs` rejects edges closer than detent × 0.4 — **except while
  aligning**, where only the jam timeout bounds the run.
- Jam timeout = detent × 2.5, measured as the **remaining** budget since the
  last counted click. A jam brakes, discards everything pending, reports
  `jammed`; nothing gates on the flag, the next click clears it.
- **The feeder task owns the motor and switch; `feeder::Feeder` decides.**
  Time comes in as ms per call; the switch level at start is the one fact the
  task supplies. Producers `try_send` portion counts on the feed channel and
  log a discard. **That channel is a branch of the `select`, never drained
  before it**, or requests arriving mid-turn are lost (ADR-0009).
- No stop between portions; mid-turn requests extend the turn.
- New calibration applies at the next idle moment, never mid-turn.

`feeder.rs` host tests cover: starts pressed, starts free, bounce at t=0, no
clicks at all, and bounce not postponing the jam. Keep them.

## Time and schedule

- **The unit owns its schedule (flash) and its clock (DS3231)**; no NTP
  (ADR-0001). A new unit starts blank, and blank must stay visible.
- **Trust ladder** (ADR-0003): live `feeder/time` / RTC with `OSF` clear / set
  by hand arm the schedule; a retained time only starts an untrusted clock;
  once trusted, retained times and RTC reads are ignored. Power-cycled with no
  trustworthy time → wait, never guess.
- **Never double-feed** (ADR-0002): consumed marker keyed on
  `(day, minute-of-day)`; baseline pass once a schedule has arrived; 2-minute
  lateness limit; only a later date re-arms; paused slots are consumed.
- Offsets are **kept, not converted**; slots are local wall-clock time. Home
  Assistant's live time wins; the unit follows its own zone after
  `HA_WINS_MS` (10 min) without one (ADR-0016).
- A meal is a **position** (ADR-0018): zero portions = off, keeps its place.

## Flash

`partitions.csv` (via `espflash.toml`): `nvs` 24 KB at 0x9000, which `espflash`
never rewrites, so configuration survives a reflash; two 1.9 MB app slots and
`otadata` (ADR-0022). Every USB flash passes `--erase-data-parts ota`, or the
bootloader boots the slot `otadata` names, not the one just written. The
bootloader is ours, with rollback (ADR-0024): `bootloader/bootloader.bin`,
rebuilt only by `dev/bootloader.sh`.
(esp-radio's `NVS` symbol is an unrelated RAM array.)

| Offset | Magic | Holds |
|---|---|---|
| nvs+0x0000 | `FDR2` | network, broker, detent interval, portion scale |
| nvs+0x1000 | `FDS1` | the schedule |
| nvs+0x2000 | `FDZ1` | the timezone name and rule |
| nvs+0x3000 | `FDP1` | the pause (ADR-0023) |

Each record has a magic and a CRC-32, so erased flash and an interrupted write
read as *unconfigured*. A missing `nvs` partition is a loud error.

- Credentials come **only** from flash (ADR-0004). `build.rs` injects one
  value, `ap_secret`. Check: `strings` on the ELF finds no Wi-Fi password.
- Boot: usable record → station; otherwise setup mode. **No fallback to setup
  after failing to connect** (ADR-0005). Reset gestures call
  `Record::without_network` (keeps calibration); only *Factory reset* calls
  `Store::erase_all`.
- `Store::update` rewrites a record with everything else untouched; an
  unchanged value is not rewritten.

## Setup mode and the admin page

`setup.rs` (never returns; reboots after saving) and `web.rs` (station mode)
both serve over `http.rs`; the pure halves are `provisioning.rs` and
`admin.rs`.

- SSID `cat-feeder-<id>`, password `base32(sha256("<ap_secret>:<id>"))` as
  `XXXX-XXXX-XXXX` (Crockford), address 192.168.4.1 from
  `provisioning::AP_ADDR_OCTETS` — the one constant the socket, the panel and a
  test all use. `dev/ap-password.sh` must agree byte for byte.
- Form fields must match `record_from_form`: `wifi_ssid`, `wifi_password`,
  `mqtt_host`, `mqtt_port`, `mqtt_user`, `mqtt_password`. `mqtt_host` is IPv4
  only (ADR-0019). `provisioning::trimmed` strips whitespace from SSID, host,
  port and user — **never from passwords**.
- Admin page rules (ADR-0017): Basic auth with the derived password; foreign
  `Origin` on POST → 403; stored passwords never rendered, empty = keep;
  network changes and `/update` restart, nothing else does.
- `POST /update` (ADR-0022) streams an image into the slot not running
  (`firmware.rs`) and selects it only if `update::ImageCheck` passes: magic,
  trailing SHA-256, and `web::SECRET_MARK` — this build's `ap_secret`, so an
  image from another `cfg.toml` is refused. Refused mid-turn; while it writes
  (`Bus::flash_busy`, until `otadata` is written) no turn starts, and the
  restart waits for held feeds.
  A new image confirms itself on the broker or resets after `CONFIRM_SECS`.
- `CONNECTIONS = 3`: browsers open several sockets; one socket gets the real
  request RST'd.
- `display_task` is spawned **before** `setup::run`, which never returns.

## MQTT contract

Details and exact discovery payloads: the `ha-mqtt-discovery` skill.

```
feeder/<id>/availability       online | offline        retained, LWT offline
feeder/<id>/feed               <portions:u8>           cmd
feeder/all/feed                <portions:u8>           cmd, every unit
feeder/<id>/paused             ON | OFF                cmd (ADR-0023)
feeder/<id>/schedule           [{"time":"08:00","portions":2}, ...]  cmd
feeder/<id>/schedule/state     same shape               retained echo
feeder/<id>/meal/<n>/time      08:00:00                cmd, n = 1..8
feeder/<id>/meal/<n>/portions  2                       cmd, 0 = off
feeder/time                    2026-09-14T08:00:00+02:00  retained, from HA
feeder/time/request            <id>                    cmd to HA
feeder/<id>/state              {"feeding","jammed","paused","meals","last_fed"}
feeder/<id>/event              {"event_type","portions","slot","at"}  not retained
```

- **Commands are never retained.** A retained command replayed at subscribe
  time is refused (schedule, meal edits); one forwarded live is acted on.
- The subscription leaves `retain_as_published` off — that is what makes
  live vs retained time distinguishable.
- Connect sequence: connect, discovery (retained), `online`, subscribe, echo
  the schedule, then `feeder/time/request` once — after subscribing, or the
  answer is missed. TCP connect and CONNACK each time out after 10 s.
- `discovery.rs`'s device `model` is a contract with the Home Assistant
  package, which finds units by it.
- `last_fed` and events are in **portions as requested**. Feeds sent *from*
  Home Assistant raise no event.

| Constant | Value | Limits |
|---|---|---|
| `schedule::MAX_SLOTS` | 8 | meals per day; a longer schedule is rejected whole |
| `portions::MAX_CLICKS` | 16 | clicks owed at once, clamped with a warning |
| `wiring::FEED_DEPTH` | 8 | unread feed *requests* |

## The knob, panel and LED

Pure logic: `button.rs` (holds, taps), `menu.rs` (pages and menu), `encoder.rs`,
`display.rs` (what the six lines say, when the panel sleeps), `indicator.rs`
(LED priority and patterns). Rules (ADR-0010, ADR-0011):

- Hold `button::ARM_HOLD_MS` toggles locked/unlocked; tap acts; turning never
  dispenses. Unlock always lands on `Feed`; 10 s idle locks. A lock needs a
  different press from the unlock. Waking shows home.
- **The unit owns its pause** (ADR-0023): the menu, the admin page and a live
  `paused` command all go through `Bus::set_pause` — flash, then in force.
- The menu reads `Bus::calibration` before every input, so a web save is not
  overwritten by a stale copy.
- Hold hints come from `display::hold_hint_for`, rounded **up**.
- A jam keeps `** JAMMED **` on the panel and offers `Retry feed`; the LED
  stays solid red over the armed state.
- No page shows a password; `display::UnitInfo` has no field for one.

## Gotchas

- `espflash write-bin` does not erase, and NOR flash only clears bits:
  `erase-region` first (`provision.sh` does). Otherwise records AND together.
- `AccessPointConfig::default()` is an **open** network; set `Wpa2Personal`.
  Applying the config starts Wi-Fi; there is no separate start call.
- `edge-dhcp` is a codec, not a server. `Server::new` defaults its pool to
  .50–.200; set `range_start`/`range_end`. Advertise the unit as gateway; no DNS.
- Log association separately from DHCP, or "never joined" and "DHCP broken"
  look identical.
- `StationConfig` defaults to `ScanMethod::Fast`, which joins the first mesh
  node, not the best; `main.rs` sets `AllChannels`.
- smoltcp's DHCP `discover_timeout` defaults to 10 s and the first DISCOVER is
  lost after association; `dhcp_config()` sets 2 s.
- `esp-storage` is pinned at 0.9; 0.10 needs an esp-hal release candidate.
- `embassy-sync` 0.8, for `embassy-embedded-hal`'s shared I²C.
- A `--no-reset` flash halts the app so only the bootloader prints; use the
  dev scripts.
- Wired buttons with both legs in one breadboard row group read
  `currently pressed` at boot and erase the network every boot.
- An SH1106 panel (common on 1.3") renders shifted by two pixels with an
  SSD1306 driver.

## Toolchain and testing

Rust stable, `riscv32imac-unknown-none-elf`, `espflash`. No espup, no
probe-rs/defmt, no BLE. Check `Cargo.toml`/`Cargo.lock` versions and use the
`esp-hal-api` skill before writing any esp-*/embassy-* call — the API shifts
between releases.

```sh
cargo test --lib --target "$(rustc -vV | awk '/^host:/{print $2}')"  # pure logic
./dev/flash.sh --board zero [--headless] [--seconds n] [--filter re]  # build, flash, capture
./dev/capture.sh [--reset]                # listen without reflashing; --reset: from boot
./dev/provision.sh [--host ip --user u --password-file -] [--detent-ms n] [--portion-scale p]
./dev/soak.sh --hours n && ./dev/soak-report.sh
./dev/pcb-check.sh                        # after every pcb.diy edit; pcb-review skill
docker compose up -d && ./dev/watch.sh    # local broker + HA; see dev/README.md
```

The explicit test target is required: `.cargo/config.toml` points cargo at
the board. The host build depends on two things:

- `src/lib.rs` gates every hardware module on `#[cfg(target_os = "none")]`;
  new pure logic goes **above** the gate. If it needs a peripheral it is not
  pure logic.
- Every esp-*/embassy-* dependency is under
  `[target.'cfg(target_os = "none")'.dependencies]`.

`build.rs` emits the linker args only for the bare-metal target, and uses a
placeholder `ap_secret` when `cfg.toml` is absent **and** `CI` is set.

## Conventions

- `no_std`, `#![no_main]`; `heapless` collections; `esp-alloc` only for
  esp-radio. Logging via `log::{info,warn,error}` — the only debug channel.
- Hardware behind traits (`MotorDriver`, `ClickSource`) so logic is
  host-tested with fakes.
- Small, flash-testable changes, each verifiable on the console.
- **Dev scripts take flags, and a flag wins over its environment variable**
  (`--board`, `--port`, `--host`, `--user`, `--password`, `--nvs-offset`,
  `--seconds`, `--filter`, `--hours`). An `ENV=value` prefix defeats permission
  allow-rules. See `dev/_common.sh` and the `dev-script` skill.
- **When a constant becomes configurable, grep the whole repo for its old
  value** — log strings, doc comments, READMEs, skill transcripts. Use the
  `drift-check` agent.
- Don't add features the roadmap doesn't call for without asking.

## Code organisation

```
src/bin/main.rs  wiring: peripherals, tasks, executor, clock following, zone
board.rs         pin map per board feature
-- pure, host-tested --
feeder.rs        align, spacing, count, brake, jam; DEBOUNCE_MS
portions.rs      pending clicks, MAX_CLICKS, portions -> clicks
calibrate.rs     the Run calibration measurement
schedule.rs      Schedule, LocalClock, Scheduler, double-feed guard
tz.rs            POSIX rules, offsets, the stored zone
reset.rs         BOOT 5 s hold
button.rs menu.rs encoder.rs display.rs indicator.rs
events.rs        feeder/<id>/event payloads
discovery.rs     Home Assistant discovery configs, JSON-checked
ds3231.rs        DS3231 registers
provisioning.rs  flash record, setup identity, form, minimal HTTP parsing
admin.rs         admin page: auth, forms, rendering
update.rs        checking an uploaded firmware image as it streams in
dhcp.rs sha256.rs
-- hardware --
motor.rs switch.rs led.rs oled.rs rtc.rs i2c.rs store.rs
mqtt.rs setup.rs http.rs web.rs
firmware.rs      the OTA slots and otadata: idle slot, write, select, confirm
wiring.rs        the Bus static: every shared handle and who writes it
config.rs        Config from the flash record
examples/mkrecord.rs  host-only record builder for provision.sh
homeassistant/packages/cat_feeder.yaml  the HA half: time, pause, copy schedule
pcb.diy          the perfboard (component side); docs/hardware.md
```

Tasks: `net`, `mqtt`, `switch`, `feeder`, `schedule` (1 s tick), `rtc`,
`encoder`, `ui`, `display`, `indicator`, `web`, `reset`, `watchdog` (feeds the
RTC watchdog; a stall or panic resets the chip in 5 s), `confirm` (only on a
new image's first boot). `encoder`, `ui` and
`display` are left out of headless builds. They communicate only through
`wiring::Bus`.
