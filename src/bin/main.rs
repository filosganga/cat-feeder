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
use cat_feeder::switch::{ClickSource, Switch};
use cat_feeder::{mqtt, switch_pin};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
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

/// Portion requests, from every producer to the one task that owns the motor.
///
/// Bounded on purpose. Producers use `try_send` and log the discard, so a stuck
/// automation can never block the MQTT or clock task waiting for room.
static FEED: Channel<CriticalSectionRawMutex, u8, 8> = Channel::new();

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

/// Milliseconds since boot, the clock the feeder state machine runs on.
fn now_ms() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
}

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
    spawner.spawn(bench_feed_requests().expect("failed to create bench task"));

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

    mqtt::run(stack, cfg, id.as_str()).await
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
        match feeder.action(now_ms()) {
            Action::Idle => {
                motor.brake();

                // Watch clicks even while idle, so a hub turned by hand is
                // visible rather than silently discarded.
                let portions = match select(FEED.receive(), CLICKS.receive()).await {
                    Either::First(portions) => portions,
                    Either::Second(()) => {
                        info!("feed: click while idle, hub turned by hand");
                        continue;
                    }
                };

                feeder.request(portions);
                if feeder.pending() == 0 {
                    continue;
                }

                let pressed = SWITCH_PRESSED.load(Ordering::Relaxed);
                feeder.start(now_ms(), pressed);
                motor.run_forward();

                log_start(feeder.pending(), pressed);
            }

            Action::Turning { jam_timeout_ms } => {
                // Absorb anything that arrived mid-turn without blocking, so
                // the motor never stops between portions.
                while let Ok(extra) = FEED.try_receive() {
                    feeder.request(extra);
                    info!("feed: pending={}", feeder.pending());
                }

                match select(CLICKS.receive(), Timer::after_millis(jam_timeout_ms as u64)).await {
                    Either::First(()) => log_click(feeder.on_click(now_ms())),
                    Either::Second(()) => {
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

/// TEMPORARY bench harness: asks for one portion every 20 s.
///
/// Exists only so the feeding loop can be exercised before the MQTT `feed`
/// subscription lands. **Delete this task when it does.**
#[embassy_executor::task]
async fn bench_feed_requests() {
    loop {
        Timer::after(Duration::from_secs(20)).await;
        match FEED.try_send(1) {
            Ok(()) => info!("bench: requested 1 portion"),
            Err(_) => warn!("bench: feed queue full, request dropped"),
        }
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
