#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use cat_feeder::config::{DEVICE_ID_LEN, device_id, load_config};
use cat_feeder::feeder::{Action, ClickOutcome, Feeder};
use cat_feeder::motor::{LogMotor, MotorDriver};
use cat_feeder::portions::{Added, MAX_PORTIONS};
use cat_feeder::schedule::{Alignment, Due, LocalClock, Scheduler, Skipped, Wall};
use cat_feeder::switch::{ClickSource, Switch};
use cat_feeder::wiring::{Bus, now_ms};
use cat_feeder::{mqtt, switch_pin};
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

    let cfg = load_config();
    let id = mk_static!(heapless::String<DEVICE_ID_LEN>, device_id());
    info!("board: devkit, id={id}");

    let switch = Switch::new(switch_pin!(peripherals));
    spawner.spawn(switch_task(switch).expect("failed to create switch task"));
    spawner.spawn(feeder_task(LogMotor::new()).expect("failed to create feeder task"));
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
        "switch: watching GPIO11, currently {}",
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

/// Owns the motor and decides nothing.
///
/// Every rule belongs to `feeder::Feeder`, which is pure and host-tested. This
/// task performs the action it is told to and reports what happened.
#[embassy_executor::task]
async fn feeder_task(mut motor: LogMotor) {
    let mut feeder = Feeder::new();

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
            log_alignment(clock.align(sync.monotonic_ms, sync.wall), sync.wall);
        }

        if let Some(schedule) = BUS.schedule.try_take() {
            info!("schedule: {} slots", schedule.len());
            scheduler.set_schedule(schedule);
        }

        // No time means no schedule. A unit that was power-cycled while the
        // broker was down waits to be told; it never guesses.
        match clock.now(now_ms()) {
            None => {
                if !waiting_logged {
                    info!("clock: no time received, waiting");
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
    match alignment {
        Alignment::Started => info!("clock: started, {wall}"),
        // Home Assistant republishes every minute, so a second or two of drift
        // is the normal state of affairs and not worth a line each time.
        Alignment::Adjusted { drift_s } if drift_s.abs() < 2 => {}
        Alignment::Adjusted { drift_s } => info!("clock: aligned, drift={drift_s}s"),
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
/// Reaching `MAX_PORTIONS` means something upstream is repeating itself — a
/// stuck automation, or a QoS 1 redelivery — and the clamp is the only thing
/// standing between that and an empty hopper. Dropping portions silently would
/// hide the fault that caused it.
///
/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_clamp(added: Added) {
    if let Added::Clamped { dropped } = added {
        warn!("feed: clamped at {MAX_PORTIONS} portions, {dropped} dropped");
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
                let _ = controller.wait_for_disconnect_async().await;
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
