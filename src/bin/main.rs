#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use cat_feeder::button::{
    BOOT_RESET_HOLD_MS, Button as ButtonGesture, Event as ButtonEvent, held_at_boot,
};
use cat_feeder::config::{AP_SECRET, Config, DEVICE_ID_LEN, device_id, load_config};
use cat_feeder::feeder::{Action, ClickOutcome, Feeder};
use cat_feeder::indicator::{Indicator, Rgb, Status};
use cat_feeder::led::Led;
use cat_feeder::motor::{LogMotor, MotorDriver};
use cat_feeder::portions::{Added, MAX_CLICKS};
use cat_feeder::provisioning::{DecodeError, Record, ap_password, ap_ssid};
use cat_feeder::schedule::{Alignment, Change, Due, LocalClock, Scheduler, Skipped, Wall};
use cat_feeder::store::{Store, StoreError};
use cat_feeder::switch::{ClickSource, Switch};
use cat_feeder::wiring::{Bus, now_ms};
use cat_feeder::{button_pin, led_pin, mqtt, switch_pin};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{
    Config as WifiConfig, ControllerConfig, Interface, WifiController, sta::StationConfig,
};
use log::{info, warn};

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

/// Supplies the clock for `esp-println`'s `timestamp` feature.
///
/// Every log line is then stamped with milliseconds since boot, which is what
/// makes the timing rules checkable on the console rather than inferred:
/// the 30 ms debounce, the 800 ms minimum click spacing, ~1.9 s per portion
/// and the 5 s jam timeout.
///
/// Lives in the binary rather than the library so the linker cannot drop it.
#[unsafe(no_mangle)]
pub extern "Rust" fn _esp_println_timestamp() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
}

/// Everything the tasks share. See `wiring.rs` for who writes what.
static BUS: Bus = Bus::new();

/// Debounced clicks, from the task that owns the switch to the feeder.
///
/// The switch gets its own task for a reason found the hard way: awaiting the
/// GPIO edge inside a `select` alongside other work means the future is created
/// and dropped repeatedly, and an edge arriving while no future is armed is
/// lost. A dedicated task holds one `next_click` future at a time and never
/// drops it, which is the shape that proved reliable on hardware.
static CLICKS: Channel<CriticalSectionRawMutex, (), 4> = Channel::new();

/// The switch's settled level, maintained by `switch_task`.
///
/// The feeder needs it to decide whether a run must align first, and a wrong
/// answer is expensive: believing the hub is off a detent when it is on one
/// spends a real quarter turn on alignment and dispenses a portion more than it
/// counts.
static SWITCH_PRESSED: AtomicBool = AtomicBool::new(false);

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write($val);
        x
    }};
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // esp-radio allocates from the heap, so both regions are needed.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 36 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    let id = mk_static!(heapless::String<DEVICE_ID_LEN>, device_id());
    info!("board: {}, id={id}", cat_feeder::board::NAME);

    // Before anything that can fail or hang, so the LED is already reporting
    // while flash is read and the network comes up. A board with no working
    // RMT still feeds cats, so this is a warning and not a panic.
    match Led::new(peripherals.RMT, led_pin!(peripherals)) {
        Ok(led) => {
            spawner.spawn(indicator_task(led).expect("failed to create indicator task"));
        }
        Err(e) => warn!("led: unavailable ({e:?}), running without an indicator"),
    }

    // Before the record is read, so a wipe simply means the normal boot path
    // finds nothing — no reboot needed, because nothing has been decided yet.
    let button = Switch::new(button_pin!(peripherals));
    let wipe = reset_held_at_boot(&button).await;

    let cfg = resolve_config(peripherals.FLASH, id, wipe);

    spawner.spawn(button_task(button).expect("failed to create button task"));

    let switch = Switch::new(switch_pin!(peripherals));
    spawner.spawn(switch_task(switch).expect("failed to create switch task"));
    spawner.spawn(feeder_task(LogMotor::new(), cfg).expect("failed to create feeder task"));
    spawner.spawn(schedule_task().expect("failed to create schedule task"));

    let station = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(cfg.wifi_ssid)
            .with_password(cfg.wifi_password.into()),
    );

    let (controller, interfaces) = esp_radio::wifi::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station),
    )
    .expect("Failed to initialize Wi-Fi controller");

    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        interfaces.station,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    );

    // In embassy-executor 0.10 the `task` macro returns a Result, so the token
    // is unwrapped before it reaches `spawn`.
    spawner.spawn(wifi_task(controller, cfg.wifi_ssid).expect("failed to create wifi task"));
    spawner.spawn(net_task(runner).expect("failed to create net task"));

    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        info!("wifi: connected, ip={}", v4.address);
    }

    mqtt::run(stack, cfg, id.as_str(), &BUS).await
}

/// Decides which credentials this unit runs on.
///
/// Flash is the source of truth. A unit that has been set up keeps its
/// credentials across every reflash, because `espflash` rewrites only the app
/// partition — which is what makes `cargo run` bearable during development.
///
/// **The seed-from-`cfg.toml` path below is temporary.** It exists so the flash
/// store can be exercised before the access point is built. Once setup mode
/// lands, an unconfigured unit raises its own network and serves the form
/// instead of quietly adopting whatever the binary was built with. Roadmap
/// step 9.
fn resolve_config(flash: esp_hal::peripherals::FLASH<'static>, id: &str, wipe: bool) -> Config {
    let mut store = match Store::new(flash) {
        Ok(store) => store,
        Err(e) => {
            // Without the partition there is nowhere to keep credentials, so
            // the unit can only run on what it was built with.
            warn!("store: no nvs partition ({e:?}), using build-time config");
            return load_config();
        }
    };

    let (offset, len) = store.location();
    info!("store: nvs at {offset:#x}, {len} bytes");

    if wipe {
        match store.erase() {
            // Note what this currently costs you: `seed_config` below writes
            // the build-time credentials straight back, so today a wipe is
            // visible only in the log. It becomes a real reset when setup mode
            // lands and that fallback goes — roadmap step 9.
            Ok(()) => warn!("store: erased by the boot button"),
            Err(e) => warn!("store: erase failed ({e:?})"),
        }
    }

    stored_config(&mut store).unwrap_or_else(|| seed_config(&mut store, id))
}

/// The credentials already in flash, if there are any worth using.
///
/// Its own function, never inlined, because a `Record` is 284 bytes and the
/// moves in and out of one do not get elided at this optimisation level. Three
/// of them in a single frame is past the stack budget this crate denies on.
#[allow(
    clippy::large_stack_frames,
    reason = "a Record is 284 bytes and decoding one cannot avoid building it by \
    value; a few copies land in one frame. This runs once, at boot, on main's own \
    task rather than nested inside an async frame that is held for the life of the \
    program. Verified on hardware: no stack overflow, and esp-backtrace would say so."
)]
#[inline(never)]
fn stored_config(store: &mut Store) -> Option<Config> {
    match store.load() {
        Ok(record) if record.is_usable() => {
            info!(
                "store: configured for {} via {}:{}",
                record.wifi_ssid, record.mqtt_host, record.mqtt_port
            );
            Some(Config::from_record(mk_static!(Record, record)))
        }
        Ok(_) => {
            warn!("store: record is unusable, re-seeding");
            None
        }
        Err(StoreError::Record(DecodeError::NotConfigured)) => {
            info!("store: no record yet");
            None
        }
        Err(e) => {
            warn!("store: unreadable ({e:?}), re-seeding");
            None
        }
    }
}

/// **TEMPORARY.** Writes the build-time config into flash so an unprovisioned
/// unit has something to connect with.
///
/// This is what setup mode replaces: an unconfigured unit should raise its own
/// network and ask, not quietly adopt whatever the binary was built with. It
/// exists now so the flash store can be exercised before the access point
/// exists. Roadmap step 9.
#[allow(
    clippy::large_stack_frames,
    reason = "a Record is 284 bytes and decoding one cannot avoid building it by \
    value; a few copies land in one frame. This runs once, at boot, on main's own \
    task rather than nested inside an async frame that is held for the life of the \
    program. Verified on hardware: no stack overflow, and esp-backtrace would say so."
)]
#[inline(never)]
fn seed_config(store: &mut Store, id: &str) -> Config {
    info!(
        "setup: would raise {} / {}",
        ap_ssid(id),
        ap_password(AP_SECRET, id)
    );

    let cfg = load_config();
    match cfg.to_record() {
        Some(record) => match store.save(&record) {
            Ok(()) => info!("store: seeded from cfg.toml"),
            Err(e) => warn!("store: could not save ({e:?})"),
        },
        None => warn!("store: cfg.toml values do not fit a record"),
    }

    cfg
}

/// Whether the button was held down through power-on, meaning "erase".
///
/// Costs one GPIO read on an ordinary boot: if the button is not already down
/// there is nothing to wait for. Only a boot that starts with it held pays the
/// three seconds, and that boot is asking for them.
///
/// Reset lives here rather than on a runtime gesture because it is the one
/// irreversible thing this button can do. Separating it from feeding by hold
/// duration alone would mean a beat too long on a working feeder wipes its
/// credentials; requiring a power cycle means it cannot happen by accident at
/// all. See `button.rs`.
async fn reset_held_at_boot(button: &Switch<'static>) -> bool {
    const INTERVAL_MS: u64 = 50;

    if !button.is_pressed() {
        return false;
    }

    info!("button: held at boot, keep holding to erase the configuration");

    // One sample past the threshold, so the loop can only decide "yes" by
    // actually observing the full duration.
    let wanted = (BOOT_RESET_HOLD_MS / INTERVAL_MS) as usize;
    let mut samples: heapless::Vec<bool, 128> = heapless::Vec::new();

    for _ in 0..wanted {
        let _ = samples.push(button.is_pressed());
        Timer::after(Duration::from_millis(INTERVAL_MS)).await;
    }

    let held = held_at_boot(samples, INTERVAL_MS);
    if !held {
        info!("button: released too early, configuration kept");
    }
    held
}

/// Owns the outside button, and decides nothing.
///
/// Every rule belongs to `button::Button`, which is pure and host-tested.
///
/// **Polls levels rather than awaiting edges**, which is the opposite of
/// `switch_task` and is deliberate. This loop has to service two sources — the
/// level changing and time passing — and `Switch::next_transition` is not
/// cancel-safe: it carries the debounce run in its own stack frame, so dropping
/// it inside a `select` resets the debounce and re-reads the level as already
/// settled, silently swallowing the transition. That is the same class of bug
/// the hub switch got its own task to avoid.
///
/// Polling is free here. The button's shortest deadline is a two-second hold,
/// so a 20 ms tick is a hundred times finer than anything it must resolve.
#[embassy_executor::task]
async fn button_task(button: Switch<'static>) {
    const TICK: Duration = Duration::from_millis(20);
    /// Consecutive equal samples before a level is believed: 40 ms.
    const STABLE: u8 = 2;

    let mut gesture = ButtonGesture::new();
    let mut settled = button.is_pressed();
    let mut candidate = settled;
    let mut stable: u8 = 0;

    info!("button: watching {}", cat_feeder::board::BUTTON_PIN);

    loop {
        Timer::after(TICK).await;
        let now = now_ms();

        let level = button.is_pressed();
        if level == candidate {
            stable = stable.saturating_add(1);
        } else {
            candidate = level;
            stable = 1;
        }

        if candidate != settled && stable >= STABLE {
            settled = candidate;
            if let Some(event) = gesture.on_change(now, settled) {
                on_button(event);
            }
        }

        // Time-driven events: arming fires while the button is still held, and
        // the window lapses with nothing pressed at all. Neither is an edge.
        if let Some(event) = gesture.poll(now) {
            on_button(event);
        }

        BUS.button_armed
            .store(gesture.is_armed(), Ordering::Relaxed);
    }
}

/// See [`log_start`] for why this is a separate, never-inlined function.
#[inline(never)]
fn on_button(event: ButtonEvent) {
    match event {
        ButtonEvent::Armed => info!("button: armed, tap to feed"),
        ButtonEvent::Expired => info!("button: locked again"),
        ButtonEvent::Locked => info!("button: tap ignored, hold 2s to arm first"),
        ButtonEvent::Feed => match BUS.feed.try_send(1) {
            Ok(()) => info!("button: feed 1"),
            Err(_) => warn!("button: feed queue full, portion dropped"),
        },
    }
}

/// Owns the switch and nothing else.
///
/// Its only job is to hold a `next_click` future continuously and forward every
/// debounced click. It must not await anything else, or an edge can arrive
/// while no future is armed and be lost.
///
/// Also publishes the switch's resting level once at boot, which is the fastest
/// way to spot a miswired button.
#[embassy_executor::task]
async fn switch_task(mut switch: Switch<'static>) {
    let pressed = switch.is_pressed();
    SWITCH_PRESSED.store(pressed, Ordering::Relaxed);
    info!(
        "switch: watching {}, currently {}",
        cat_feeder::board::SWITCH_PIN,
        if pressed { "pressed" } else { "released" }
    );

    loop {
        let pressed = switch.next_transition().await;
        SWITCH_PRESSED.store(pressed, Ordering::Relaxed);

        // Pressed means the contact just closed, which is the falling edge the
        // feeder counts. The release half of the cycle only updates the level.
        if pressed && CLICKS.try_send(()).is_err() {
            warn!("switch: click dropped, feeder is not keeping up");
        }
    }
}

/// Owns the RGB LED and decides nothing.
///
/// Every rule belongs to `indicator::Indicator`, which is pure and host-tested
/// down to the blink timing. This task samples, renders, and writes.
#[embassy_executor::task]
async fn indicator_task(mut led: Led<'static>) {
    led_selftest(&mut led).await;

    let mut indicator = Indicator::new();
    let mut shown: Option<Rgb> = None;
    let mut said: Option<Status> = None;

    loop {
        let colour = indicator.poll(now_ms(), BUS.health());

        // Only on a change. The tick is fast enough to render a 120 ms flash
        // cleanly, which would otherwise mean ~40 pointless RMT transmissions a
        // second for an LED that is dark most of its life.
        if shown != Some(colour) {
            led.set(colour).await;
            shown = Some(colour);
        }

        if indicator.status() != said {
            said = indicator.status();
            log_indicator(said);
        }

        Timer::after(INDICATOR_TICK).await;
    }
}

/// A red, green, blue sweep at power-on, naming each colour as it shows it.
///
/// Two jobs, and the second is why this is permanent rather than a diagnostic
/// that got left in.
///
/// **It proves the LED works.** Dark is the healthy state, which means a dead
/// LED, a broken solder joint or a wrong pin look exactly like a unit with
/// nothing to report. A sweep at boot is the only moment that distinction is
/// ever made, and it costs half a second.
///
/// **It pins the channel order.** The WS2812B datasheet says GRB; the dev kit's
/// LED wanted RGB, and the whole palette came out inverted until a run of this
/// found it. The two boards are not guaranteed to carry the same part, so run
/// it on a Zero before trusting any colour there: read the three console lines
/// against the board, and if they disagree, `led::wire_word` is the one place
/// to fix it.
///
/// Full brightness rather than the dim palette, because naming a colour
/// confidently is the entire point and a dim primary is harder to be sure of.
async fn led_selftest(led: &mut Led<'static>) {
    for (name, colour) in [
        ("red", Rgb::new(60, 0, 0)),
        ("green", Rgb::new(0, 60, 0)),
        ("blue", Rgb::new(0, 0, 60)),
    ] {
        info!("led: selftest {name}");
        led.set(colour).await;
        Timer::after(SELFTEST_HOLD).await;
    }
}

/// Long enough to name a colour, short enough not to be in the way.
///
/// Was two seconds while the channel order was in question. That is a long time
/// to watch on every reflash, and 200 ms is still unmistakable when you know
/// three colours are coming.
const SELFTEST_HOLD: Duration = Duration::from_millis(200);

/// How often the LED is re-rendered.
///
/// Has to divide a flash finely enough that its edges land where
/// `indicator::Pattern` says they do. At 25 ms there are ~5 samples per 120 ms
/// flash, so the worst-case edge error is well below anything an eye resolves.
const INDICATOR_TICK: Duration = Duration::from_millis(25);

/// See [`log_start`] for why this is a separate, never-inlined function.
///
/// Worth a console line of its own: it is how the LED's behaviour gets checked
/// during bring-up, and how a blink code seen across the room can be confirmed
/// against what the firmware believed it was showing.
#[inline(never)]
fn log_indicator(status: Option<Status>) {
    if let Some(status) = status {
        info!("led: {status:?}");
    }
}

/// Owns the motor and decides nothing.
///
/// Every rule belongs to `feeder::Feeder`, which is pure and host-tested. This
/// task performs the action it is told to and reports what happened.
#[embassy_executor::task]
async fn feeder_task(mut motor: LogMotor, cfg: Config) {
    let mut feeder = Feeder::new(cfg.timings, cfg.portion_scale_pct);
    log_calibration(cfg);

    loop {
        let action = feeder.action(now_ms());

        // Published by `mqtt`. Set from the action about to be taken, so the
        // state topic never claims the unit is feeding while it waits for work.
        BUS.status
            .set(matches!(action, Action::Turning { .. }), feeder.is_jammed());

        match action {
            Action::Idle => {
                motor.brake();

                // Watch clicks even while idle, so an edge nobody asked for is
                // visible rather than silently discarded.
                //
                // On the bench that is the test button. On an assembled feeder
                // it means the hub was turned by hand — possible, but it takes
                // real effort against the gear reduction — or that the switch
                // is noisy. Either is worth seeing.
                let portions = match select(BUS.feed.receive(), CLICKS.receive()).await {
                    Either::First(portions) => portions,
                    Either::Second(()) => {
                        info!("feed: click while idle, nothing was feeding");
                        continue;
                    }
                };

                log_clamp(feeder.request(portions));
                if feeder.pending() == 0 {
                    continue;
                }

                let pressed = SWITCH_PRESSED.load(Ordering::Relaxed);
                feeder.start(now_ms(), pressed);
                motor.run_forward();

                log_start(feeder.pending(), pressed);
            }

            Action::Turning { jam_timeout_ms } => {
                // A request arriving mid-turn must wake this loop, not sit in
                // the queue until something else does. Draining the channel at
                // the top of the loop is not enough: the loop then blocks for
                // the whole jam budget, and with a real motor the request is
                // only seen at the next click — by which time the portion has
                // finished, the machine has gone idle and the motor has braked.
                // That is the stop-and-restart the design forbids.
                //
                // Both receives are cancel-safe: the messages live in the
                // channels, so the losing future can be dropped without losing
                // anything. That is the whole reason clicks travel by channel.
                let event = select3(
                    CLICKS.receive(),
                    BUS.feed.receive(),
                    Timer::after_millis(jam_timeout_ms as u64),
                )
                .await;

                match event {
                    Either3::First(()) => log_click(feeder.on_click(now_ms())),
                    Either3::Second(extra) => {
                        // No `start`, no touching the motor: it is already
                        // turning, and this only lengthens the same run.
                        log_clamp(feeder.request(extra));
                        info!("feed: pending={}", feeder.pending());
                    }
                    Either3::Third(()) => {
                        motor.brake();
                        feeder.on_timeout();
                        warn!("feed: no click for 5s, jammed; pending discarded");
                    }
                }
            }
        }
    }
}

/// What this unit was calibrated for, once at boot.
///
/// Worth a line: these come from the record in flash and differ per unit, so a
/// feeder behaving oddly is either mis-measured or mis-provisioned, and this is
/// the only place that distinction is visible.
///
/// See [`log_start`] for why it is a separate, never-inlined function — adding
/// this `info!` inline put `feeder_task` over the crate's stack budget, which
/// is exactly what `deny(clippy::large_stack_frames)` is there to catch.
#[inline(never)]
fn log_calibration(cfg: Config) {
    info!(
        "feeder: clicks >{} ms apart, jam after {} ms, portions x{}%",
        cfg.timings.min_click_spacing_ms, cfg.timings.jam_timeout_ms, cfg.portion_scale_pct
    );
}

/// Kept out of `feeder_task` and never inlined.
///
/// Every `info!` site contributes its formatting temporaries to the enclosing
/// frame, and an async task's frame is allocated for the whole life of the
/// future. Inlining these puts the task over the project's stack budget.
#[inline(never)]
fn log_start(portions: u8, switch_pressed: bool) {
    info!(
        "feed: start, portions={portions}{}",
        if switch_pressed {
            ""
        } else {
            ", needs aligning"
        }
    );
}

/// Owns the clock and the schedule, and decides nothing either.
///
/// Every rule belongs to `schedule::Scheduler`, which is pure and host-tested.
/// This task applies whatever `mqtt` last heard from the broker, asks once a
/// second whether anything is due, and forwards the answer to the feeder.
#[embassy_executor::task]
async fn schedule_task() {
    let mut clock = LocalClock::new();
    let mut scheduler = Scheduler::new();
    let mut waiting_logged = false;

    loop {
        if let Some(sync) = BUS.time.try_take() {
            let alignment = clock.align(sync.monotonic_ms, sync.wall, sync.source);
            log_alignment(alignment, sync.wall);
        }

        if let Some(schedule) = BUS.schedule.try_take() {
            info!("schedule: {} slots", schedule.len());
            scheduler.set_schedule(schedule);
        }

        // Armed means a *live* time has arrived, not merely that the clock is
        // running. A unit holding on a retained time will not feed, and this is
        // the only thing that says so outside the serial console.
        BUS.net.set_armed(clock.is_trusted());

        // No trustworthy time means no schedule. A unit power-cycled while the
        // broker was down waits to be told; so does one handed only a retained
        // time, which may be whatever Home Assistant published before it
        // stopped. Both wait; neither guesses.
        match clock.now(now_ms()).filter(|_| clock.is_trusted()) {
            None => {
                if !waiting_logged {
                    info!("clock: no trusted time yet, schedule holding");
                    waiting_logged = true;
                }
            }
            Some(now) => match scheduler.next_due(now, BUS.is_paused()) {
                Due::Nothing => {}
                Due::Feed {
                    minute_of_day,
                    portions,
                } => {
                    // Recorded only once the request is queued, so a full queue
                    // never leaves `last_fed` claiming a meal that never ran.
                    match BUS.feed.try_send(portions) {
                        Ok(()) => {
                            BUS.last_fed.set(now);
                            log_due(minute_of_day, portions);
                        }
                        Err(_) => warn!("schedule: feed queue full, slot dropped"),
                    }
                }
                Due::Consumed { minute_of_day, why } => log_skipped(minute_of_day, why),
            },
        }

        Timer::after(SCHEDULE_TICK).await;
    }
}

/// How often the scheduler looks at the clock.
///
/// Anything under a minute is enough, since the lateness limit is two minutes.
/// A second keeps a slot's log line close to its wall-clock time, which makes
/// the console readable against Home Assistant's own history.
const SCHEDULE_TICK: Duration = Duration::from_secs(1);

/// See [`log_start`] for why these are separate, never-inlined functions.
/// The startup line prints the time in full, offset included.
///
/// The offset never enters a feeding decision — slots are local wall-clock
/// times and the feeders share a house with the broker, so there is nothing to
/// convert. It is printed because it is the one clue that the assumption has
/// broken: an automation publishing `utcnow()` instead of `now()` would still
/// look like a valid time while moving every meal by the offset.
#[inline(never)]
fn log_alignment(alignment: Alignment, wall: Wall) {
    if alignment.armed_now {
        info!("clock: live time {wall}, schedule armed");
        return;
    }

    match alignment.change {
        Change::Started if alignment.trusted => info!("clock: started, {wall}"),
        // Worth a line of its own. Until a live message lands, this unit is
        // running but will not feed on schedule, and nothing else says so.
        Change::Started => info!("clock: started, {wall} (retained; waiting for a live time)"),
        Change::IgnoredStale => info!("clock: ignored a retained time, still on the live one"),
        // Home Assistant republishes every minute, so a second or two of drift
        // is the normal state of affairs and not worth a line each time. The
        // first alignment after boot is usually larger: it is the age of the
        // retained message the unit started from, not the crystal.
        Change::Adjusted { drift_s } if drift_s.abs() < 2 => {}
        Change::Adjusted { drift_s } => info!("clock: aligned, drift={drift_s}s"),
    }
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_due(minute_of_day: u16, portions: u8) {
    info!(
        "schedule: slot {:02}:{:02} due, feeding {portions}",
        minute_of_day / 60,
        minute_of_day % 60
    );
}

/// See [`log_start`] for why this is a separate function.
///
/// Every one of these lines is a meal that did *not* happen, so none of them is
/// silent. A feeder that quietly stops feeding is the failure mode this whole
/// project has to avoid.
#[inline(never)]
fn log_skipped(minute_of_day: u16, why: Skipped) {
    let hour = minute_of_day / 60;
    let minute = minute_of_day % 60;
    match why {
        Skipped::Baseline => info!("schedule: slot {hour:02}:{minute:02} already past at startup"),
        Skipped::Paused => {
            info!("schedule: slot {hour:02}:{minute:02} due but paused, marking consumed")
        }
        Skipped::TooLate { by_s } => info!(
            "schedule: slot {hour:02}:{minute:02} missed by {}m, not catching up",
            by_s / 60
        ),
    }
}

/// Says out loud when the hopper guard bit.
///
/// Reaching `MAX_CLICKS` means something upstream is repeating itself — a
/// stuck automation, or a QoS 1 redelivery — and the clamp is the only thing
/// standing between that and an empty hopper. Dropping portions silently would
/// hide the fault that caused it.
///
/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_clamp(added: Added) {
    if let Added::Clamped { dropped } = added {
        warn!("feed: clamped at {MAX_CLICKS} portions, {dropped} dropped");
    }
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_click(outcome: ClickOutcome) {
    match outcome {
        ClickOutcome::Aligned => info!("feed: aligned"),
        ClickOutcome::Counted { remaining: 0 } => info!("feed: done"),
        ClickOutcome::Counted { remaining } => info!("feed: click, {remaining} to go"),
        ClickOutcome::TooSoon => info!("feed: edge ignored, below 800ms minimum spacing"),
        ClickOutcome::NotTurning => {}
    }
}

/// Keeps the station associated, retrying forever. Losing Wi-Fi is normal.
#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>, ssid: &'static str) {
    loop {
        info!("wifi: connecting to {ssid}");

        // Deliberately not logging the association info or the disconnect
        // reason: formatting those types costs more stack than this task is
        // allowed, and neither has been worth the space so far.
        match controller.connect_async().await {
            Ok(_) => {
                info!("wifi: associated");
                BUS.net.set_link(true);
                let _ = controller.wait_for_disconnect_async().await;
                BUS.net.set_link(false);
                warn!("wifi: disconnected");
            }
            Err(e) => warn!("wifi: connect failed {e:?}"),
        }

        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}
