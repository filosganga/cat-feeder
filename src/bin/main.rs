#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use cat_feeder::config::{DEVICE_ID_LEN, device_id, load_config};
use cat_feeder::switch::{ClickSource, Switch};
use cat_feeder::{mqtt, switch_pin};
use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
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
    spawner
        .spawn(switch_task(switch).expect("failed to create switch task"));

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
    spawner.spawn(
        wifi_task(controller, cfg.wifi_ssid).expect("failed to create wifi task"),
    );
    spawner.spawn(net_task(runner).expect("failed to create net task"));

    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        info!("wifi: connected, ip={}", v4.address);
    }

    mqtt::run(stack, cfg, id.as_str()).await
}

/// Roadmap step 2: count clicks on the console so the switch and the debounce
/// can be checked by hand, before any motor exists.
#[embassy_executor::task]
async fn switch_task(mut switch: Switch<'static>) {
    info!(
        "switch: waiting for clicks on GPIO11, currently {}",
        if switch.is_pressed() {
            "pressed"
        } else {
            "released"
        }
    );

    let mut clicks: u32 = 0;
    loop {
        switch.next_click().await;
        clicks += 1;
        info!("switch: click {clicks}");
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
