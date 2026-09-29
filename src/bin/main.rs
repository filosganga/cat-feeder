#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]
// The headless build leaves out the knob, the menu and the panel, and the
// functions only they call. Gating each of those individually would thread
// `cfg` through a third of this file; an unused function is harmless.
#![cfg_attr(
    feature = "headless",
    allow(dead_code, unused_imports, unused_variables)
)]

#[cfg(not(feature = "headless"))]
use cat_feeder::button::{BOOT_RESET_HOLD_MS, held_at_boot};
use cat_feeder::calibrate::{
    Failure as CalibrationFailure, Measurement, Progress, Run as CalibrationRun,
};
use cat_feeder::config::{AP_SECRET, Config, DEVICE_ID_LEN, device_id};
use cat_feeder::display::{self, Fed, Net, Screen, SetupInfo, UnitInfo, View};
use cat_feeder::ds3231::Reading as RtcReading;
use cat_feeder::encoder::Decoder;
use cat_feeder::feeder::{Action, ClickOutcome, Feeder, Timings};
use cat_feeder::i2c::Bus as I2cBus;
use cat_feeder::indicator::{Indicator, Rgb, Status};
use cat_feeder::led::Led;
use cat_feeder::menu::{
    Calibration, ClockEdit, Field, Item, Menu, Mode as MenuMode, Outcome as MenuOutcome, Page,
    Setting,
};
use cat_feeder::motor::{Drv8833, MotorDriver};
use cat_feeder::oled::Oled;
use cat_feeder::portions::{Added, MAX_CLICKS};
use cat_feeder::provisioning::{
    AP_PASSWORD_LEN, AP_SSID_LEN, DecodeError, Record, ap_password, ap_ssid,
};
use cat_feeder::reset::{Hold, HoldToReset};
use cat_feeder::rtc::Rtc;
use cat_feeder::schedule::{
    Alignment, Change, Due, LocalClock, Schedule, ScheduleCommand, ScheduleRecordError, Scheduler,
    Skipped, SlotChange, SlotEdit, TimeSource, Wall, seconds_between,
};
use cat_feeder::store::{SharedStore, Store, StoreError};
use cat_feeder::switch::{ClickSource, Switch};
use cat_feeder::tz::{Zone, ZoneRecordError};
use cat_feeder::wiring::{Bus, TimeSync, now_ms};
use cat_feeder::{
    boot_button_pin, button_pin, display_scl_pin, display_sda_pin, encoder_a_pin, encoder_b_pin,
    led_pin, motor_in1_pin, motor_in2_pin, motor_sleep_pin, mqtt, switch_pin,
};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::{
    Config as WifiConfig, ControllerConfig, Interface, WifiController,
    sta::{ScanMethod, StationConfig},
};
use log::{error, info, warn};

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
    #[cfg(not(feature = "headless"))]
    let button = Switch::new(button_pin!(peripherals));
    #[cfg(not(feature = "headless"))]
    let wipe = reset_held_at_boot(&button).await;
    // Headless: GPIO3 is not wired, so it is not read. A stray bridge to ground
    // there must not forget the network on every boot; the BOOT hold is this
    // build's reset.
    #[cfg(feature = "headless")]
    let wipe = false;

    // The knob's two lines, pulled up like every other contact to ground on
    // this board. Spawned on both boot paths with the click, so a unit in
    // setup mode still answers the knob on the console.
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    #[cfg(not(feature = "headless"))]
    let (encoder_a, encoder_b) = (
        Input::new(encoder_a_pin!(peripherals), pull_up),
        Input::new(encoder_b_pin!(peripherals), pull_up),
    );

    // The BOOT button, on both builds: held five seconds while running, it
    // forgets the network settings. See `reset.rs` and `reset_task`.
    let boot_button = Input::new(boot_button_pin!(peripherals), pull_up);

    // Built here rather than after the branch below, so that setup mode can
    // report its level too. A unit in setup mode is a unit on a bench being
    // wired, which is exactly when knowing what this pin reads is worth most.
    let switch = Switch::new(switch_pin!(peripherals));

    // Read flash **first**, so the recovery path cannot be held up by an
    // optional output. The panel is brought up between the decision and acting
    // on it, rather than before both: `Oled::new` probes two I²C addresses, and
    // a bus shorted by a hand-soldered jumper — the recurring mistake on these
    // boards — leaves that probe waiting on the peripheral's own timeout. In
    // front of this call it would delay a reset gesture forgetting the network and
    // the access point coming up, which is the one way back into a unit nobody
    // can reach. Behind it, the worst case is a slow boot with a warning.
    let boot = resolve_config(peripherals.FLASH, wipe);

    // After the decision, before acting on it, because **both** outcomes want a
    // screen and one of them can never come back to build it: `setup::run` does
    // not return, so it could not spawn `display_task` afterwards.
    //
    // The panel is optional either way: a feeder with no screen still feeds
    // cats, so a missing or miswired one is a warning and `display_task` runs
    // regardless, reporting to the console alone.
    //
    // `mk_static!` rather than moving it into the task: an `Oled` is over a kilobyte,
    // almost all of it the frame buffer, and an async task's frame is live for
    // the whole life of the future. Passing it by value put `display_task` at
    // 1268 bytes against the crate's 1024 budget. A buffer that lives forever
    // belongs in a static; the task carries a pointer.
    //
    // The bus is built here and shared: the RTC is on the same two wires, and
    // gets its own handle further down. See `i2c.rs`.
    let i2c_bus: Option<&'static I2cBus> = match cat_feeder::i2c::bus(
        peripherals.I2C0,
        display_sda_pin!(peripherals),
        display_scl_pin!(peripherals),
    ) {
        Ok(bus) => Some(mk_static!(I2cBus, bus)),
        Err(e) => {
            warn!("i2c: bus would not configure ({e:?}); no panel, no RTC");
            None
        }
    };
    // Headless: no panel to probe. The bus stays, for the RTC.
    #[cfg(feature = "headless")]
    let oled: Option<&'static mut Oled<'static>> = None;
    #[cfg(not(feature = "headless"))]
    let oled: Option<&'static mut Oled<'static>> = match i2c_bus {
        Some(bus) => match Oled::new(I2cDevice::new(bus)).await {
            Ok(oled) => Some(mk_static!(Oled<'static>, oled)),
            Err(e) => {
                warn!("oled: no panel ({e:?}), showing the screen on the console only");
                None
            }
        },
        None => None,
    };

    // No usable record means setup mode, and setup mode never returns. It is
    // entered before any task that assumes a network, because there is not
    // going to be one.
    let (cfg, store) = match boot {
        Boot::Configured(cfg, store) => (cfg, store),
        Boot::Setup(store) => {
            BUS.setup.store(true, Ordering::Relaxed);
            // No knob in setup mode. The setup screen replaces every page and
            // the menu, so a hold would turn the LED cyan behind a menu nobody
            // can see, and its items would act on a unit with no broker and
            // no feeder task. The power-on gesture above, if this build has one, has
            // already run.
            let _ = boot_button;
            #[cfg(not(feature = "headless"))]
            let _ = (button, encoder_a, encoder_b);
            log_setup_switch_level(&switch);

            // Derived once, here, and handed to both the screen and the radio.
            // `ap_ssid` and `ap_password` are pure and deterministic, so calling
            // them twice would not drift — but one derivation makes it obvious
            // that the panel shows the password the network actually has, which
            // is the entire claim this screen makes.
            let ssid = mk_static!(heapless::String<AP_SSID_LEN>, ap_ssid(id));
            let password = mk_static!(
                heapless::String<AP_PASSWORD_LEN>,
                ap_password(AP_SECRET, id)
            );

            // Headless: the console above and the sticker are all there is,
            // which is why `dev/ap-password.sh` exists.
            #[cfg(not(feature = "headless"))]
            spawner.spawn(
                display_task(
                    oled,
                    Some(SetupInfo {
                        ssid: ssid.as_str(),
                        password: password.as_str(),
                    }),
                    None,
                )
                .expect("failed to create display task"),
            );

            cat_feeder::setup::run(
                spawner,
                peripherals.WIFI,
                ssid.as_str(),
                password.as_str(),
                store,
            )
            .await
        }
        Boot::Unconfigurable => halt_unconfigurable(oled).await,
    };

    // The store goes to the ui task, which is the only writer a configured unit
    // has: the knob's settings save through it, and the factory reset erases
    // through it.
    //
    // Shared behind a lock, because the schedule task writes through it too:
    // a schedule command is stored before it is put in force. The meals are
    // read here, once, before either task can write.
    let mut store = store;
    let meals = load_schedule(&mut store);
    BUS.zone.set(load_zone(&mut store));
    let store = mk_static!(SharedStore, SharedStore::new(store));
    let calibration = Calibration {
        portion_scale_pct: cfg.portion_scale_pct,
        detent_ms: cfg.detent_ms,
    };
    BUS.calibration.set(calibration);
    #[cfg(not(feature = "headless"))]
    {
        spawner.spawn(ui_task(button, store, calibration).expect("failed to create ui task"));
        spawner.spawn(encoder_task(encoder_a, encoder_b).expect("failed to create encoder task"));
    }
    #[cfg(feature = "headless")]
    {
        info!("board: headless, no knob and no panel");
    }
    spawner.spawn(reset_task(boot_button, store).expect("failed to create reset task"));

    spawner.spawn(switch_task(switch).expect("failed to create switch task"));

    // The real bridge. Its inputs and nSLEEP all have internal pull-downs, so
    // the motor stayed coasting from power-on until this line ran.
    let motor = Drv8833::new(
        motor_in1_pin!(peripherals),
        motor_in2_pin!(peripherals),
        motor_sleep_pin!(peripherals),
    );
    spawner.spawn(feeder_task(motor, cfg).expect("failed to create feeder task"));
    spawner.spawn(schedule_task(store, meals).expect("failed to create schedule task"));
    if let Some(bus) = i2c_bus {
        spawner.spawn(rtc_task(Rtc::new(I2cDevice::new(bus))).expect("failed to create rtc task"));
    }
    // `None` for setup: a configured unit has no setup network to describe.
    // What it has instead is a configuration, which the info pages show.
    let unit = UnitInfo {
        id: id.as_str(),
        board: cat_feeder::board::NAME,
        version: env!("CARGO_PKG_VERSION"),
        wifi_ssid: cfg.wifi_ssid,
        mqtt_host: cfg.mqtt_host,
        mqtt_port: cfg.mqtt_port,
        mqtt_user: cfg.mqtt_user,
    };
    #[cfg(not(feature = "headless"))]
    spawner.spawn(display_task(oled, None, Some(unit)).expect("failed to create display task"));

    let station = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(cfg.wifi_ssid)
            .with_password(cfg.wifi_password.into())
            // Scan every channel and join the strongest node, not the first
            // one heard. `Fast` is the default and stops at the first match,
            // which on a mesh network can be a node across the house: seen at
            // -82 dBm a metre from another node. `WIFI_CONNECT_AP_BY_SIGNAL`
            // is already set by esp-radio but only applies to a full scan.
            .with_scan_method(ScanMethod::AllChannels),
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
        embassy_net::Config::dhcpv4(dhcp_config()),
        // DHCP, MQTT and the admin page's connections, with one spare.
        mk_static!(StationSockets, StationSockets::new()),
        seed,
    );

    // In embassy-executor 0.10 the `task` macro returns a Result, so the token
    // is unwrapped before it reaches `spawn`.
    spawner.spawn(wifi_task(controller, cfg.wifi_ssid).expect("failed to create wifi task"));
    spawner.spawn(net_task(runner).expect("failed to create net task"));

    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        info!("wifi: connected, ip={}", v4.address);
        BUS.ip.set(Some(v4.address.address().octets()));
    }

    // The admin page. Its password is the one the setup network had — derived,
    // never stored, so there is nothing to leak from flash and nothing to reset
    // but the record itself.
    let password = mk_static!(
        heapless::String<AP_PASSWORD_LEN>,
        ap_password(AP_SECRET, id)
    );
    let admin = cat_feeder::web::Unit {
        id: id.as_str(),
        version: env!("CARGO_PKG_VERSION"),
        password: password.as_str(),
        network: cat_feeder::admin::Network {
            wifi_ssid: cfg.wifi_ssid,
            mqtt_host: cfg.mqtt_host,
            mqtt_port: cfg.mqtt_port,
            mqtt_user: cfg.mqtt_user,
        },
    };
    spawner.spawn(web_task(stack, store, admin).expect("failed to create web task"));

    mqtt::run(stack, cfg, id.as_str(), &BUS).await
}

/// Decides which credentials this unit runs on, or that there are none.
///
/// Flash is the only source. A unit that has been set up keeps its credentials
/// across every reflash, because `espflash` rewrites only the app partition —
/// which is what makes `cargo run` bearable during development.
///
/// [`Boot::Setup`] means setup mode. There is deliberately **no build-time
/// fallback** any more: credentials compiled into the binary were what roadmap
/// step 9 set out to remove, and `dev/provision.sh` writes a record over USB
/// without a compiler — so an unconfigured board is never stranded. It either
/// gets a record from the host or asks for one over its own network.
///
/// The `Store` travels into setup mode rather than being dropped here, because
/// the form has to write what it is given back to the same partition this read.
fn resolve_config(flash: esp_hal::peripherals::FLASH<'static>, wipe: bool) -> Boot {
    let mut store = match Store::new(flash) {
        Ok(store) => store,
        Err(e) => {
            // Without the partition there is nowhere to keep credentials, so
            // this unit cannot be configured by either route. That is a build
            // or flashing fault rather than a runtime condition, and setup mode
            // would be a lie — it could not save what it was given.
            error!("store: no nvs partition ({e:?}); this unit cannot be configured");
            return Boot::Unconfigurable;
        }
    };

    let (offset, len) = store.location();
    info!("store: nvs at {offset:#x}, {len} bytes");

    if wipe {
        match store.forget_network() {
            Ok(()) => warn!("store: network forgotten by the boot button, calibration kept"),
            Err(e) => warn!("store: forgetting the network failed ({e:?})"),
        }
    }

    match stored_config(&mut store) {
        Some(cfg) => Boot::Configured(cfg, store),
        None => Boot::Setup(store),
    }
}

/// What the boot path found in flash.
enum Boot {
    /// A usable record. Run normally, keeping the `Store` for the knob's
    /// settings and its factory reset.
    Configured(Config, Store),
    /// Writable flash with nothing usable in it. Setup mode, carrying the
    /// `Store` the form will save through.
    Setup(Store),
    /// No `nvs` partition at all, so there is nowhere a record could go.
    Unconfigurable,
}

/// Stops, for a unit that cannot be configured by any route.
///
/// Deliberately **not** setup mode. With no partition to write, the form would
/// raise a network, take somebody's Wi-Fi password, and fail at the last step —
/// and the reset button could not help either, because there is nothing to
/// erase. Saying so once and stopping is the honest answer; the fix is a build
/// or flashing one, not something the firmware can do at runtime.
///
/// **The panel is turned off on the way**, and it is the reason this takes an
/// argument at all. `Oled::new` ends its initialisation sequence with the
/// display on and deliberately blanked, so a unit that stopped here would sit
/// lit and empty forever — a signal that means nothing, in the one state
/// nothing can recover from. Dark at least means what it looks like.
///
/// The blanking is `Oled::new`'s and is recent: before it, "lit and blank" was
/// simply untrue. An SSD1306's display RAM powers up undefined, so the panel
/// showed scattered pixels rather than nothing at all, and this comment
/// described a screen the hardware never produced. There is no screen worth drawing instead:
/// nobody standing at the feeder can fix a missing partition, and the console
/// already says which one it is.
async fn halt_unconfigurable(oled: Option<&'static mut Oled<'static>>) -> ! {
    error!("store: refusing setup mode, because a record could not be saved");

    if let Some(oled) = oled {
        oled.set_power(false).await;
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

/// The credentials already in flash, if there are any worth using.
///
/// Its own function, never inlined, because a `Record` is a few hundred bytes
/// and the moves in and out of one do not get elided at this optimisation
/// level. Three of them in a single frame is past the stack budget this crate
/// denies on.
#[allow(
    clippy::large_stack_frames,
    reason = "a Record is a few hundred bytes and decoding one cannot avoid building \
    it by value; a few copies land in one frame. This runs once, at boot, on main's \
    own task rather than nested inside an async frame held for the life of the \
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
        // Every path below lands in setup mode rather than guessing. A unit
        // that believes a half-written record sits trying to join a network
        // that does not exist, with no way back but the button.
        Ok(record) if record.is_network_forgotten() => {
            info!("store: no network in the record (calibration kept), going to setup");
            None
        }
        Ok(_) => {
            warn!("store: record is unusable, going to setup");
            None
        }
        Err(StoreError::Record(DecodeError::NotConfigured)) => {
            info!("store: no record yet, going to setup");
            None
        }
        Err(e) => {
            warn!("store: unreadable ({e:?}), going to setup");
            None
        }
    }
}

/// The BOOT button's reset: five seconds held, and the network settings go.
///
/// Sampled every 50 ms; the rules are `reset.rs`'s. The record is rewritten by
/// `Record::without_network` — the power-on gesture's forgetting, not the
/// menu's factory reset — so the unit comes back in setup mode with its meals,
/// calibration and timezone.
#[embassy_executor::task]
async fn reset_task(boot: Input<'static>, store: &'static SharedStore) {
    let mut hold = HoldToReset::new();
    let mut counting = false;
    loop {
        Timer::after(Duration::from_millis(50)).await;
        match hold.update(now_ms(), boot.is_low()) {
            Hold::Idle => {
                if counting {
                    info!("reset: released, network settings kept");
                }
                counting = false;
                BUS.reset_held.store(false, Ordering::Relaxed);
            }
            Hold::Counting { .. } => {
                if !counting {
                    info!("reset: BOOT held, keep holding to forget the network settings");
                }
                counting = true;
                BUS.reset_held.store(true, Ordering::Relaxed);
            }
            Hold::Reset => {
                match store.lock().await.forget_network() {
                    Ok(()) => {
                        warn!("reset: network forgotten, calibration kept, restarting into setup")
                    }
                    Err(e) => {
                        warn!(
                            "reset: forgetting the network failed ({e:?}), nothing changed; let go and hold again"
                        );
                        // Not "released": the button is still down.
                        counting = false;
                        BUS.reset_held.store(false, Ordering::Relaxed);
                        continue;
                    }
                }
                // Long enough for the console line to leave the USB buffer.
                Timer::after(Duration::from_millis(250)).await;
                esp_hal::system::software_reset();
            }
        }
    }
}

/// Whether the button was held down through power-on, meaning "forget the
/// network". Not on the headless build, which has no knob to hold.
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
#[cfg(not(feature = "headless"))]
async fn reset_held_at_boot(button: &Switch<'static>) -> bool {
    const INTERVAL_MS: u64 = 50;

    if !button.is_pressed() {
        return false;
    }

    info!("button: held at boot, keep holding to forget the network settings");

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
        info!("button: released too early, network settings kept");
    }
    held
}

/// Owns the knob's click, takes the knob's turns, and decides nothing.
///
/// Every rule belongs to `menu::Menu`, which is pure and host-tested.
///
/// **Polls the click rather than awaiting edges**, which is the opposite of
/// `switch_task` and is deliberate. This loop has to service three sources —
/// the level changing, time passing, and turns arriving — and
/// `Switch::next_transition` is not cancel-safe: it carries the debounce run in
/// its own stack frame, so dropping it inside a `select` resets the debounce
/// and re-reads the level as already settled, silently swallowing the
/// transition. That is the same class of bug the hub switch got its own task
/// to avoid.
///
/// Polling is free here. The shortest deadline is a two-second hold, so a
/// 20 ms tick is a hundred times finer than anything it must resolve. The tick
/// is a fixed deadline rather than a fresh 20 ms after every wake, so a knob
/// spun fast cannot starve the click of samples.
#[embassy_executor::task]
async fn ui_task(button: Switch<'static>, store: &'static SharedStore, calibration: Calibration) {
    const TICK: Duration = Duration::from_millis(20);

    let mut menu = Menu::new(calibration);
    menu.set_now(BUS.now.get());
    let mut click = Click::new(button.is_pressed());
    let mut next_tick = Instant::now() + TICK;

    // The level, not just the pin, exactly as `switch_task` reports it. A
    // button stuck at ground is indistinguishable from a working one until the
    // console says which it is, and the boot-gesture line only appears when the
    // pin already reads pressed — so a fault looks like silence.
    log_button_level(click.settled);

    // Only the waiting lives in this frame. Everything an input means is
    // decided in `ui_input`, out of line, so the menu's outcomes and their
    // temporaries are never held across an `.await` — an async task's frame
    // lasts as long as the task, and this one kept outgrowing its budget.
    loop {
        let input = match select(Timer::at(next_tick), TURNS.receive()).await {
            Either::First(()) => {
                next_tick += TICK;
                UiInput::Tick(button.is_pressed())
            }
            Either::Second(steps) => UiInput::Turn(steps),
        };

        // Only the two outcomes that write flash come back, because only they
        // need the store's lock, which is an `.await`.
        if let Some(outcome) = ui_input(&mut menu, &mut click, input) {
            if with_store(&mut menu, &mut *store.lock().await, outcome) {
                // Only the factory reset restarts. Long enough for the console
                // line to leave the USB buffer.
                Timer::after(Duration::from_millis(250)).await;
                esp_hal::system::software_reset();
            }
            BUS.mode.set(menu.mode());
            BUS.redraw.signal(());
        }
    }
}

/// What woke the ui task.
enum UiInput {
    /// The 20 ms tick, with the click's level sampled on it.
    Tick(bool),
    /// The knob moved this many detents.
    Turn(i8),
}

/// The click's debounce: consecutive equal samples before a level is believed.
struct Click {
    settled: bool,
    candidate: bool,
    stable: u8,
}

impl Click {
    /// 40 ms at the 20 ms tick.
    const STABLE: u8 = 2;

    fn new(level: bool) -> Self {
        Self {
            settled: level,
            candidate: level,
            stable: 0,
        }
    }

    /// A fresh sample. Returns the new settled level when it changes.
    fn sample(&mut self, level: bool) -> Option<bool> {
        if level == self.candidate {
            self.stable = self.stable.saturating_add(1);
        } else {
            self.candidate = level;
            self.stable = 1;
        }
        if self.candidate != self.settled && self.stable >= Self::STABLE {
            self.settled = self.candidate;
            return Some(self.settled);
        }
        None
    }
}

/// Everything one input means, carried out. Returns an outcome only if it
/// needs the store — see `ui_task`.
#[inline(never)]
fn ui_input(menu: &mut Menu, click: &mut Click, input: UiInput) -> Option<MenuOutcome> {
    let now = now_ms();
    // The calibration in force, before anything edits or saves it: the admin
    // page may have changed it since the last input.
    menu.set_calibration(BUS.calibration.get());
    let outcome = match input {
        UiInput::Tick(level) => {
            let mut outcome = None;
            if let Some(pressed) = click.sample(level) {
                // Any press wakes the screen, whatever the menu makes of it.
                // Deliberately on the press rather than the release, so the
                // panel is already lit by the time a finger lifts.
                if pressed {
                    BUS.last_press.set(now);
                }
                outcome = menu.on_change(now, pressed);
            }
            // Time-driven: a hold unlocks while still held, and the window
            // lapses with nothing touched. Neither is an edge.
            outcome.or_else(|| menu.poll(now))
        }
        UiInput::Turn(steps) => {
            // Whether the panel was lit *before* this turn, which decides
            // whether the turn steps a page or only wakes the screen.
            let awake = display::awake(Status::of(BUS.health()), now, BUS.last_press.get());
            BUS.last_press.set(now);
            menu.on_turn(now, steps, awake)
        }
    };
    // A calibration run reports from the feeder task; picked up on the next
    // tick, which at 20 ms is far finer than a detent.
    let outcome = outcome.or_else(|| calibration_news(menu));

    BUS.button_armed
        .store(menu.is_unlocked(), Ordering::Relaxed);
    // For the next input: the clock editor opens on the time now.
    menu.set_now(BUS.now.get());

    let outcome = outcome?;
    if matches!(
        outcome,
        MenuOutcome::Save { .. } | MenuOutcome::FactoryReset
    ) {
        return Some(outcome);
    }
    on_menu(outcome);
    BUS.mode.set(menu.mode());
    BUS.redraw.signal(());
    None
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_button_level(pressed: bool) {
    info!(
        "button: watching {}, currently {}",
        cat_feeder::board::BUTTON_PIN,
        if pressed { "pressed" } else { "released" }
    );
}

/// Carries out what the menu decided, and says so.
///
/// See [`log_start`] for why this is a separate, never-inlined function.
#[inline(never)]
fn on_menu(outcome: MenuOutcome) {
    match outcome {
        // Both need the store, and are handled in `ui_task` with its lock.
        MenuOutcome::Save { .. } | MenuOutcome::FactoryReset => {}
        MenuOutcome::SetClock(wall) => set_clock(wall),
        MenuOutcome::StartCalibration => {
            BUS.calibrate.signal(());
            info!("menu: calibration started");
        }
        MenuOutcome::Unlocked => info!("menu: unlocked, turn to choose, tap to run"),
        // The two ways back to locked, named apart on purpose: one is a
        // decision and the other is ten seconds passing, and a console that
        // called both "locked again" could not tell you which happened.
        MenuOutcome::Locked => info!("menu: locked"),
        MenuOutcome::Expired => info!("menu: locked, window lapsed"),
        MenuOutcome::Home => info!("menu: home"),
        MenuOutcome::Woke => info!("menu: woke the screen"),
        MenuOutcome::Moved(mode) => log_moved(mode),
        MenuOutcome::Feed => match BUS.feed.try_send(1) {
            Ok(()) => info!("menu: feed 1"),
            Err(_) => warn!("menu: feed queue full, portion dropped"),
        },
        MenuOutcome::TogglePause => toggle_pause(),
    }
}

/// The knob set the time. See `Bus::set_clock_by_hand`.
#[inline(never)]
fn set_clock(wall: Wall) {
    BUS.set_clock_by_hand(wall);
    info!("menu: clock set by hand to {wall}");
}

/// Applied locally at once, so the schedule stops now and the menu's label
/// flips under the finger, and published so the retained flag — where it
/// actually lives — agrees. See `Bus::pause_request`.
#[inline(never)]
fn toggle_pause() {
    let paused = !BUS.is_paused();
    BUS.set_paused(paused);
    BUS.pause_request.signal(paused);
    info!(
        "menu: schedule {}",
        if paused { "paused" } else { "resumed" }
    );
}

/// The menu outcomes that write flash. True means restart now.
#[inline(never)]
fn with_store(menu: &mut Menu, store: &mut Store, outcome: MenuOutcome) -> bool {
    match outcome {
        MenuOutcome::Save { field, value } => {
            apply_save(menu, store, field, value);
            false
        }
        MenuOutcome::FactoryReset => factory_reset(store),
        _ => false,
    }
}

/// Everything the unit was told: credentials, calibration and meals. It comes
/// back up in setup mode by the one path that already exists. Wider than the
/// boot gesture's erase, which keeps the meals — see `store.rs`. True means
/// restart now.
#[inline(never)]
fn factory_reset(store: &mut Store) -> bool {
    match store.erase_all() {
        Ok(()) => {
            warn!("menu: configuration and meals erased, restarting into setup");
            true
        }
        Err(e) => {
            warn!("menu: erase failed ({e:?}), nothing changed");
            false
        }
    }
}

/// Stores a figure the knob chose and puts it in force, without a restart.
///
/// Flash first, then the menu and the bus, so nothing claims a value the
/// record does not hold. The feeder task picks it up from `BUS.calibration` at
/// its next idle moment — never mid-turn, see `Feeder::recalibrate`.
#[inline(never)]
fn apply_save(menu: &mut Menu, store: &mut Store, field: Field, value: u16) {
    if save_setting(store, field, value) {
        menu.saved(field, value);
        BUS.calibration.set(menu.calibration());
    }
}

/// Writes one calibration figure into the record, keeping everything else.
///
/// Read back from flash rather than rebuilt from `Config`, so the credentials
/// are written back exactly as they were stored. True means it landed; a
/// failure changes nothing and says so.
#[inline(never)]
fn save_setting(store: &mut Store, field: Field, value: u16) -> bool {
    let updated = store.update(|record| match field {
        Field::PortionScale => record.portion_scale_pct = value,
        Field::Detent => record.detent_ms = value,
    });
    match updated {
        Ok(()) => {
            let (name, unit) = match field {
                Field::PortionScale => ("portion scale", "%"),
                Field::Detent => ("detent", "ms"),
            };
            info!("menu: saved {name} {value}{unit}");
            true
        }
        Err(e) => {
            warn!("menu: save failed ({e:?}), nothing changed");
            false
        }
    }
}

/// Where a turn landed, by name rather than by `Debug`, which costs a frame.
#[inline(never)]
fn log_moved(mode: MenuMode) {
    let (what, name) = match mode {
        MenuMode::Locked { page } => (
            "page",
            match page {
                Page::Home => "home",
                Page::Network => "network",
                Page::Broker => "broker",
                Page::Device => "device",
            },
        ),
        MenuMode::Unlocked { item } => (
            "cursor on",
            match item {
                Item::Feed => "feed",
                Item::Pause => "pause",
                Item::Settings => "settings",
                Item::Lock => "lock",
            },
        ),
        MenuMode::Settings { item } => (
            "settings, cursor on",
            match item {
                Setting::Clock => "clock",
                Setting::PortionScale => "portion",
                Setting::Detent => "detent",
                Setting::Calibrate => "calibrate",
                Setting::Reset => "reset",
                Setting::Back => "back",
            },
        ),
        MenuMode::Editing { field, value } => {
            let name = match field {
                Field::PortionScale => "portion",
                Field::Detent => "detent",
            };
            info!("menu: editing {name}, {value}");
            return;
        }
        MenuMode::ConfirmReset { erase } => ("reset?", if erase { "erase" } else { "keep" }),
        MenuMode::SettingClock(edit) => return log_clock_edit(edit),
        MenuMode::ConfirmCalibrate { start } => {
            ("calibrate?", if start { "start" } else { "keep" })
        }
        MenuMode::Calibrating { clicks } => return log_calibrating(clicks),
        // `calibrate:` already logged what the run measured.
        MenuMode::Calibrated(_) => ("calibration", "shown"),
    };
    info!("menu: {what} {name}");
}

/// Out of [`log_moved`], whose frame the formatting would otherwise push over
/// the stack budget.
#[inline(never)]
fn log_clock_edit(edit: ClockEdit) {
    info!(
        "menu: clock {:04}-{:02}-{:02} {:02}:{:02}, on the {:?}",
        edit.year, edit.month, edit.day, edit.hour, edit.minute, edit.field
    );
}

/// See [`log_clock_edit`].
#[inline(never)]
fn log_calibrating(clicks: u8) {
    info!("menu: calibrating, click {clicks}");
}

/// Progress or the end of a calibration run, handed to the menu.
#[inline(never)]
fn calibration_news(menu: &mut Menu) -> Option<MenuOutcome> {
    let now = now_ms();
    if let Some(result) = BUS.calibration_result.try_take() {
        return menu.calibration_finished(now, result);
    }
    BUS.calibration_clicks
        .try_take()
        .and_then(|clicks| menu.calibration_progress(now, clicks))
}

/// Turns of the knob, from the task sampling it to the one that decides.
static TURNS: Channel<CriticalSectionRawMutex, i8, 8> = Channel::new();

/// Samples the encoder's two lines and forwards whole detents.
///
/// **Polled every millisecond**, not interrupt-driven. The decoder in
/// `encoder.rs` needs every intermediate state to know the direction, and a
/// knob spun briskly changes state every few milliseconds, so a millisecond is
/// comfortably inside that. Two pins read in one wake also cannot tear the way
/// two separately awaited edges can. The cost is a thousand short wakes a
/// second, which the executor does not notice.
#[embassy_executor::task]
async fn encoder_task(a: Input<'static>, b: Input<'static>) {
    const SAMPLE: Duration = Duration::from_millis(1);

    let mut decoder = Decoder::new(
        a.is_high(),
        b.is_high(),
        cat_feeder::board::ENCODER_REVERSED,
        cat_feeder::board::ENCODER_HALF_STEP,
    );
    info!(
        "encoder: watching {}/{}, currently {}{}",
        cat_feeder::board::ENCODER_A_PIN,
        cat_feeder::board::ENCODER_B_PIN,
        a.is_high() as u8,
        b.is_high() as u8,
    );

    loop {
        Timer::after(SAMPLE).await;
        let step = decoder.update(a.is_high(), b.is_high());
        if step != 0 && TURNS.try_send(step).is_err() {
            // Only if the ui task has stalled; a detent is not worth blocking for.
            warn!("encoder: turn dropped");
        }
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
/// The switch's resting level, for the setup path only.
///
/// `switch_task` prints this on every other boot, but it is deliberately not
/// spawned in setup mode: nothing drains `CLICKS` there, so it would fill and
/// then warn about a feeder that does not exist. The level is still worth a
/// line, because setup mode is when a unit is on a bench with fresh solder on
/// it — and a miswired hub switch is otherwise invisible until the unit is
/// configured, by which point the wiring is behind a closed case.
#[inline(never)]
fn log_setup_switch_level(switch: &Switch<'static>) {
    info!(
        "switch: watching {}, currently {} (not counted yet, still in setup)",
        cat_feeder::board::SWITCH_PIN,
        if switch.is_pressed() {
            "pressed"
        } else {
            "released"
        }
    );
}

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
async fn feeder_task(mut motor: Drv8833<'static>, cfg: Config) {
    let mut feeder = Feeder::new(cfg.timings, cfg.portion_scale_pct);
    log_calibration(cfg.timings, cfg.portion_scale_pct);
    let mut applied = BUS.calibration.get();

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
                let portions =
                    match select3(BUS.feed.receive(), CLICKS.receive(), BUS.calibrate.wait()).await
                    {
                        Either3::First(portions) => portions,
                        Either3::Second(()) => {
                            info!("feed: click while idle, nothing was feeding");
                            continue;
                        }
                        // Only ever started from idle, so a run never shares the
                        // motor with a meal. Feed requests arriving meanwhile wait
                        // in the queue and run after it.
                        Either3::Third(()) => {
                            calibrate(&mut motor, feeder.is_jammed()).await;
                            continue;
                        }
                    };

                // Before the request, so a meal asked for after the knob saved a
                // new scale is counted at it. Idle here, so it always applies.
                let wanted = BUS.calibration.get();
                if wanted != applied {
                    let timings = Timings::from_detent(wanted.detent_ms);
                    if feeder.recalibrate(timings, wanted.portion_scale_pct) {
                        applied = wanted;
                        log_calibration(timings, wanted.portion_scale_pct);
                    }
                }

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
                    Either3::First(()) => {
                        log_click(feeder.on_click(now_ms()), cfg.timings.min_click_spacing_ms)
                    }
                    Either3::Second(extra) => {
                        // No `start`, no touching the motor: it is already
                        // turning, and this only lengthens the same run.
                        log_clamp(feeder.request(extra));
                        info!("feed: pending={}", feeder.pending());
                    }
                    Either3::Third(()) => {
                        motor.brake();
                        feeder.on_timeout();
                        // The configured budget, not the literal 5s this used
                        // to claim and not the `jam_timeout_ms` above, which is
                        // whatever was *left* when the wait started.
                        log_jam(cfg.timings.jam_timeout_ms);
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
/// the only place that distinction is visible. Printed at boot, and again
/// whenever the knob's settings put new figures in force.
///
/// See [`log_start`] for why it is a separate, never-inlined function — adding
/// this `info!` inline put `feeder_task` over the crate's stack budget, which
/// is exactly what `deny(clippy::large_stack_frames)` is there to catch.
#[inline(never)]
fn log_calibration(timings: Timings, portion_scale_pct: u16) {
    info!(
        "feeder: clicks >{} ms apart, jam after {} ms, portions x{}%",
        timings.min_click_spacing_ms, timings.jam_timeout_ms, portion_scale_pct
    );
}

/// One calibration run: turn, time the clicks, brake, report.
///
/// Deliberately outside `feeder::Feeder`, and without its rules. The minimum
/// click spacing and the jam budget are both derived from the detent interval
/// this run is measuring, so a badly wrong current figure could reject real
/// clicks or call a slow mechanism jammed. Only the 30 ms debounce, which is
/// the switch task's, and a fixed generous jam limit apply. See `calibrate.rs`.
///
/// Nothing here is a meal: no `last_fed`, and the schedule never hears of it.
/// The state payload does show `feeding`, because the motor is turning.
async fn calibrate(motor: &mut Drv8833<'static>, jammed: bool) {
    let mut run = CalibrationRun::new();
    BUS.calibration_progress
        .set(Progress::Running { clicks: 0 });
    BUS.status.set(true, jammed);
    motor.run_forward();

    while !run.done() {
        match select(
            CLICKS.receive(),
            Timer::after_millis(cat_feeder::calibrate::JAM_MS),
        )
        .await
        {
            Either::First(()) => {
                let clicks = run.on_click(now_ms());
                BUS.calibration_clicks.signal(clicks as u8);
                BUS.calibration_progress.set(Progress::Running {
                    clicks: clicks as u8,
                });
            }
            Either::Second(()) => break,
        }
    }

    motor.brake();
    BUS.status.set(false, jammed);
    let result = run.result();
    log_calibration_run(result);
    BUS.calibration_progress.set(Progress::Finished(result));
    BUS.calibration_result.signal(result);
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_calibration_run(result: Result<Measurement, CalibrationFailure>) {
    match result {
        Ok(m) => info!(
            "calibrate: detent {} ms (gaps {}-{} ms)",
            m.detent_ms, m.fastest_ms, m.slowest_ms
        ),
        Err(e) => warn!("calibrate: failed, {e:?}"),
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
async fn schedule_task(store: &'static SharedStore, meals: Option<Schedule>) {
    let mut clock = LocalClock::new();
    let mut scheduler = Scheduler::new();
    let mut waiting_logged = false;
    // When a live `feeder/time` last arrived. While Home Assistant publishes,
    // its offset is the authority and the stored timezone waits.
    let mut last_live_ms: Option<u64> = None;

    // What flash held at boot. `None` leaves the scheduler without a schedule
    // at all, which is what a new unit is: it never feeds, and the panel and
    // the state payload say so, until it is given one.
    if let Some(schedule) = meals {
        BUS.held.set(Some(schedule.clone()));
        scheduler.set_schedule(schedule);
    }

    loop {
        if let Some(sync) = BUS.time.try_take() {
            if sync.source == TimeSource::Live {
                last_live_ms = Some(sync.monotonic_ms);
            }
            let alignment = clock.align(sync.monotonic_ms, sync.wall, sync.source);
            log_alignment(alignment, sync.wall, sync.source);
        }

        apply_commands(&mut scheduler, store).await;

        // Armed means the clock is trusted — a live time, a DS3231 that kept
        // time, or one set by hand — not merely that it is running. A unit
        // holding on a retained time will not feed, and this is the only thing
        // that says so outside the serial console.
        BUS.net.set_armed(clock.is_trusted());

        // No trustworthy time means no schedule. A unit power-cycled with no
        // broker and an RTC that lost its time waits to be told; so does one
        // handed only a retained time, which may be whatever Home Assistant
        // published before it stopped. Both wait; neither guesses.
        if clock.is_trusted() {
            follow_zone(&mut clock, last_live_ms);
        }

        let trusted_now = clock.now(now_ms()).filter(|_| clock.is_trusted());
        BUS.now.set(trusted_now);
        match trusted_now {
            None => {
                if !waiting_logged {
                    info!("clock: no trusted time yet, schedule holding");
                    waiting_logged = true;
                }
            }
            Some(now) => resolve(&mut scheduler, now),
        }

        Timer::after(SCHEDULE_TICK).await;
    }
}

/// Every schedule command waiting, in order: a meal's time and its portions
/// are two edits, and the second applies to the first's result.
///
/// Out of line for the frame budget, as [`resolve`] is: the commands and the
/// schedules they produce would otherwise sit in `schedule_task`'s frame for
/// the life of the task.
#[inline(never)]
async fn apply_commands(scheduler: &mut Scheduler, store: &'static SharedStore) {
    while let Ok(command) = BUS.schedule.try_receive() {
        if let Some(schedule) = command_result(scheduler.schedule(), command) {
            accept_schedule(scheduler, store, schedule).await;
        }
    }
}

/// How long after the last live `feeder/time` the stored timezone takes over.
/// Home Assistant publishes every minute, so ten minutes of silence means it
/// has stopped, not that it is between ticks.
const HA_WINS_MS: u64 = 10 * 60 * 1_000;

/// Keeps the clock in the offset the stored timezone says is in force — the
/// summer-time change, on a unit nobody tells the time — and gives an offset
/// to a reading that has none, such as the knob's.
///
/// Not while Home Assistant is publishing: its live time carries the offset
/// from the tz database it ships, which is fresher than any rule stored here.
/// A change goes to the RTC too, so a restart comes back in the right frame.
#[inline(never)]
fn follow_zone(clock: &mut LocalClock, last_live_ms: Option<u64>) {
    let now = now_ms();
    if last_live_ms.is_some_and(|at| now.saturating_sub(at) < HA_WINS_MS) {
        return;
    }
    let Some(zone) = BUS.zone.get() else {
        return;
    };
    let Some(wall) = clock.now(now) else {
        return;
    };
    let Some(moved) = cat_feeder::tz::follow(&zone.parsed(), wall) else {
        return;
    };
    clock.rezone(now, moved);
    if wall.offset_minutes.is_some() {
        info!("clock: {} changed to {moved}", zone.name);
    } else {
        info!("clock: {moved}, in {}", zone.name);
    }
    BUS.rtc_time.signal(TimeSync {
        monotonic_ms: now,
        wall: moved,
        source: TimeSource::Manual,
    });
}

/// A schedule command: store it, then put it in force, then echo it.
///
/// Flash first, so what the unit acts on is what it would come back up with.
/// A failed write still puts it in force — the command is the latest word on
/// what the cats should eat, and following it in RAM beats ignoring it — but
/// says loudly that a reboot would lose it.
///
/// An unchanged schedule is not rewritten: the same broadcast sent twice, or
/// to every unit when only one needed it, costs no flash erase.
async fn accept_schedule(
    scheduler: &mut Scheduler,
    store: &'static SharedStore,
    schedule: Schedule,
) {
    if BUS.held.get().as_ref() == Some(&schedule) {
        info!("schedule: {} meals, unchanged", schedule.meals());
        return;
    }
    let stored = store.lock().await.save_schedule(&schedule);
    log_schedule_stored(schedule.meals(), stored);
    BUS.held.set(Some(schedule.clone()));
    scheduler.set_schedule(schedule);
    BUS.schedule_changed.signal(());
}

/// The schedule a command asks for, or `None` with a line saying why not.
///
/// An edit applies to what the scheduler holds — an empty schedule for a unit
/// never given one, so a blank unit can be given its first meal slot by slot.
/// A refused edit changes nothing, and the entity in Home Assistant springs
/// back when the unchanged echo does not move it.
#[inline(never)]
fn command_result(held: &Schedule, command: ScheduleCommand) -> Option<Schedule> {
    let edit = match &command {
        ScheduleCommand::Edit(edit) => Some(*edit),
        ScheduleCommand::Replace(_) => None,
    };
    match (command.apply(held), edit) {
        (Ok(schedule), edit) => {
            if let Some(edit) = edit {
                log_edit(edit);
            }
            Some(schedule)
        }
        (Err(e), edit) => {
            let meal = edit.map_or(0, |edit| edit.index + 1);
            warn!("schedule: meal {meal} edit refused: {e:?}");
            // The refusal is invisible in Home Assistant otherwise: a
            // non-optimistic entity only moves on an echo, and there is none.
            // Republishing the unchanged one puts it back.
            BUS.schedule_changed.signal(());
            None
        }
    }
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_edit(edit: SlotEdit) {
    let meal = edit.index + 1;
    match edit.change {
        SlotChange::Time(minute) => {
            info!(
                "schedule: meal {meal} at {:02}:{:02}",
                minute / 60,
                minute % 60
            )
        }
        SlotChange::Portions(0) => info!("schedule: meal {meal} switched off"),
        SlotChange::Portions(n) => info!("schedule: meal {meal} is {n} portions"),
    }
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_schedule_stored(meals: usize, stored: Result<(), StoreError>) {
    match stored {
        Ok(()) => info!("schedule: {meals} meals, stored"),
        Err(e) => {
            warn!("schedule: {meals} meals in force, but not stored ({e:?}); a reboot loses them")
        }
    }
}

/// The timezone in flash at boot, and a line saying what it was.
#[inline(never)]
fn load_zone(store: &mut Store) -> Option<Zone> {
    match store.load_zone() {
        Ok(zone) => {
            info!("clock: timezone {} ({})", zone.name, zone.rule);
            Some(zone)
        }
        Err(ZoneRecordError::NotStored) => None,
        Err(ZoneRecordError::Corrupt) => {
            warn!("clock: stored timezone is unreadable; keeping plain local time");
            None
        }
    }
}

/// The schedule in flash at boot, and a line saying what it was.
#[inline(never)]
fn load_schedule(store: &mut Store) -> Option<Schedule> {
    match store.load_schedule() {
        Ok(schedule) => {
            info!("schedule: {} meals from flash", schedule.meals());
            Some(schedule)
        }
        Err(ScheduleRecordError::NotStored) => {
            info!("schedule: none stored; this unit will not feed until given one");
            None
        }
        Err(ScheduleRecordError::Corrupt) => {
            warn!("schedule: stored record is unreadable; not feeding until given a new one");
            None
        }
    }
}

/// One tick of the schedule, given a clock that can be trusted.
///
/// Out of line, and not for tidiness: `schedule_task` was already within a few
/// dozen bytes of the crate's 1024-byte frame budget, and publishing the
/// upcoming slot pushed it over. An async task's frame is held for the whole
/// life of the future, so every `Due` temporary and every `info!` argument in
/// here would be resident forever. `deny(clippy::large_stack_frames)` caught
/// it, which is the third time that lint has paid for itself.
#[inline(never)]
fn resolve(scheduler: &mut Scheduler, now: Wall) {
    // Before resolving, and with `&self`, so the screen describes the meal that
    // is coming rather than the one this call is about to consume.
    BUS.next.set(scheduler.upcoming(now));

    match scheduler.next_due(now, BUS.is_paused()) {
        Due::Nothing => {}
        Due::Feed {
            minute_of_day,
            portions,
        } => {
            // Recorded only once the request is queued, so a full queue never
            // leaves `last_fed` claiming a meal that never ran.
            match BUS.feed.try_send(portions) {
                Ok(()) => {
                    BUS.last_fed.set(now, portions);
                    log_due(minute_of_day, portions);
                }
                Err(_) => warn!("schedule: feed queue full, slot dropped"),
            }
        }
        Due::Consumed { minute_of_day, why } => log_skipped(minute_of_day, why),
    }
}

/// Renders the screen, pushes it to the panel, and says it on the console too.
///
/// The console copy is not a leftover from before the driver existed. It is
/// what makes the *state* checkable rather than only the layout: `display::render`
/// is pure and host-tested, so what a capture proves is whether `last_fed` is
/// ever populated, whether the upcoming slot survives the trip through the bus,
/// and whether the banner tracks the same ladder the LED is showing. It is also
/// the whole screen on a unit whose panel did not answer.
///
/// **Spawned on both boot paths.** `setup` is `Some` in setup mode, where it
/// replaces the screen entirely — and that is the one state whose contents
/// exist nowhere else, because a unit cannot otherwise tell anyone the password
/// of the network it has just raised.
#[embassy_executor::task]
async fn display_task(
    mut oled: Option<&'static mut Oled<'static>>,
    setup: Option<SetupInfo<'static>>,
    unit: Option<UnitInfo<'static>>,
) {
    // In a static rather than the task's frame: six lines of twenty-one is
    // 172 bytes a copy, and an async task's frame is held for the life of the
    // future. `redraw` builds the new screen in its own short-lived frame and
    // only this pointer crosses an `.await`.
    let shown = mk_static!(Option<Screen>, None);
    let mut lit: Option<bool> = None;

    loop {
        let status = Status::of(BUS.health());

        if redraw(shown, setup, unit)
            && let (Some(oled), Some(screen)) = (oled.as_mut(), shown.as_ref())
        {
            oled.show(screen).await;
        }

        // Blanked on a timer and woken by the knob. The buffer is
        // still written while dark, so a wake shows current state rather than
        // whatever was on screen when it slept.
        let awake = display::awake(status, now_ms(), BUS.last_press.get());

        if lit != Some(awake) {
            if let Some(oled) = oled.as_mut() {
                oled.set_power(awake).await;
            }
            info!("display: {}", if awake { "awake" } else { "asleep" });
            lit = Some(awake);
        }

        // A tick for the state behind the screen, and the ui task's signal
        // for the knob, so a turn is drawn at once rather than up to a second
        // later.
        select(Timer::after(DISPLAY_TICK), BUS.redraw.wait()).await;
    }
}

/// Renders the current screen into `shown`, and says whether it changed.
///
/// Out of line so the `View` and the fresh `Screen` live in this frame and are
/// gone before `display_task` awaits the panel. See [`log_start`] for the
/// pattern.
#[inline(never)]
fn redraw(
    shown: &mut Option<Screen>,
    setup: Option<SetupInfo<'static>>,
    unit: Option<UnitInfo<'static>>,
) -> bool {
    let screen = current_screen(setup, unit);

    // Only on a change, exactly as the LED does. A screen logged every second
    // would bury every other line on the console, and the interesting thing
    // about a screen is when it changes anyway.
    if shown.as_ref() == Some(&screen) {
        return false;
    }
    log_screen(&screen);
    *shown = Some(screen);
    true
}

/// The screen as things stand, built in its own frame so the `View` it is
/// rendered from never shares one with the screen being compared.
#[inline(never)]
fn current_screen(setup: Option<SetupInfo<'static>>, unit: Option<UnitInfo<'static>>) -> Screen {
    // One snapshot, read for several fields: sampling `BUS.health()` again
    // could straddle a change and render a screen no single instant produced.
    let health = BUS.health();
    display::render(&View {
        status: Status::of(health),
        // Constant for the life of setup mode, so this renders the same screen
        // every tick and pushes none of it after the first.
        setup,
        last_fed: BUS
            .last_fed
            .get()
            .map(|(at, portions)| Fed { at, portions }),
        next: BUS.next.get(),
        mode: BUS.mode.get(),
        paused: health.paused,
        net: Net {
            link: health.link,
            broker: health.broker,
            ip: BUS.ip.get(),
        },
        unit,
        calibration: BUS.calibration.get(),
        now: BUS.now.get(),
        no_meals: BUS.held.meals() == 0,
    })
}

/// How often the screen is rebuilt.
///
/// A second, matching the schedule task that feeds it: nothing here changes
/// faster than the state behind it, and a panel refreshed more often than its
/// inputs move is just I²C traffic.
const DISPLAY_TICK: Duration = Duration::from_secs(1);

/// The rendered screen, one line per console line.
///
/// Bordered so the 21-character budget is visible at a glance: a line that
/// reaches the right-hand bar is a line that would be clipped on the panel.
#[inline(never)]
fn log_screen(screen: &Screen) {
    info!("display: +---------------------+");
    for line in screen.lines() {
        info!("display: |{line:<21}|");
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
fn log_alignment(alignment: Alignment, wall: Wall, source: TimeSource) {
    if alignment.armed_now {
        log_armed(wall, source);
        return;
    }

    match alignment.change {
        Change::Started if alignment.trusted => info!("clock: started, {wall}"),
        // Worth a line of its own. Until a live message lands, this unit is
        // running but will not feed on schedule, and nothing else says so.
        Change::Started => info!("clock: started, {wall} (retained; waiting for a live time)"),
        // Retained, or an RTC read after a live time: either way the clock is
        // already trusted and stays where it is.
        Change::IgnoredStale => info!("clock: ignored a stale time, keeping the trusted one"),
        // Home Assistant republishes every minute, so a second or two of drift
        // is the normal state of affairs and not worth a line each time. The
        // first alignment after boot is usually larger: it is the age of the
        // retained message the unit started from, not the crystal.
        Change::Adjusted { drift_s } if drift_s.abs() < 2 => {}
        Change::Adjusted { drift_s } => info!("clock: aligned, drift={drift_s}s"),
    }
}

/// Which source first armed the schedule, named apart, because "armed from the
/// RTC" and "armed from Home Assistant" mean different things about the house:
/// the first is a unit feeding without anybody publishing the time.
///
/// Out of [`log_alignment`] to keep both frames inside the stack budget.
#[inline(never)]
fn log_armed(wall: Wall, source: TimeSource) {
    match source {
        TimeSource::Rtc => info!("clock: RTC time {wall}, schedule armed"),
        TimeSource::Manual => info!("clock: set by hand to {wall}, schedule armed"),
        _ => info!("clock: live time {wall}, schedule armed"),
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
        // Clicks, not portions. The scale is applied before the cap, so on a
        // unit calibrated away from 100% these are not the same number and the
        // old wording named the wrong one.
        warn!("feed: clamped at {MAX_CLICKS} clicks, {dropped} dropped");
    }
}

/// Says how long the mechanism was given before being called stuck.
///
/// The budget is per unit, so a literal here would state a figure the firmware
/// is not using — on the one line that explains a meal not happening.
///
/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_jam(jam_timeout_ms: u64) {
    warn!("feed: no click for {jam_timeout_ms}ms, jammed; pending discarded");
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_click(outcome: ClickOutcome, min_spacing_ms: u64) {
    match outcome {
        ClickOutcome::Aligned => info!("feed: aligned"),
        ClickOutcome::Counted { remaining: 0 } => info!("feed: done"),
        ClickOutcome::Counted { remaining } => info!("feed: click, {remaining} to go"),
        // The real threshold, not a literal. It is derived from this unit's
        // detent interval, so a hardcoded number would state a figure the
        // firmware is not using — on exactly the log line someone reads when
        // clicks are going missing.
        ClickOutcome::TooSoon => {
            info!("feed: edge ignored, below {min_spacing_ms}ms minimum spacing")
        }
        ClickOutcome::NotTurning => {}
    }
}

/// DHCP as smoltcp does it, but resending DISCOVER after two seconds, not ten.
///
/// The ten was measured before it was understood: from `wifi: associated` to
/// `wifi: connected` took 10015–10059 ms on four captures in a row — the first
/// DISCOVER goes out as the link comes up, before the access point forwards
/// for us, and is lost; nothing retries until smoltcp's `discover_timeout`
/// expires. Shortening the retry rather than delaying the first DISCOVER
/// covers that cause and any other lost packet, at the cost of at most a few
/// extra broadcasts on a network that is not answering anyway.
fn dhcp_config() -> embassy_net::DhcpConfig {
    let mut config = embassy_net::DhcpConfig::default();
    config.retry_config.discover_timeout = smoltcp::time::Duration::from_secs(2);
    config
}

/// Reports what the DS3231 holds, and keeps it set from live `feeder/time`.
///
/// At boot, a trustworthy reading — oscillator never stopped since it was set
/// — goes to the schedule task as a [`TimeSource::Rtc`] time, which arms the
/// schedule without waiting for Home Assistant. A reading with `OSF` set is
/// reported and otherwise ignored.
///
/// Writes only when `ds3231::needs_set` says so — not trustworthy, or more
/// than its tolerance out — so a healthy clock is read once a minute and
/// written once. Live times only — see `Bus::rtc_time`.
#[embassy_executor::task]
async fn rtc_task(mut rtc: Rtc<'static>) {
    match rtc.read().await {
        Ok(reading) => {
            log_rtc_reading(reading);
            // A clock that never stopped since a live time set it arms the
            // schedule, so a reboot with Home Assistant down still feeds. The
            // reading is stamped now, as `mqtt.rs` stamps a message on
            // arrival. See `TimeSource::Rtc`.
            if let Some(wall) = reading.trustworthy() {
                BUS.time.signal(TimeSync {
                    monotonic_ms: now_ms(),
                    wall,
                    source: TimeSource::Rtc,
                });
            }
        }
        Err(e) => {
            log_rtc_error("nothing answered; running without one", e);
            return;
        }
    }

    let mut clock = LocalClock::new();
    loop {
        let sync = BUS.rtc_time.wait().await;
        clock.align(sync.monotonic_ms, sync.wall, TimeSource::Live);
        let Some(now) = clock.now(now_ms()) else {
            continue;
        };

        let reading = match rtc.read().await {
            Ok(reading) => reading,
            Err(e) => {
                log_rtc_error("read failed", e);
                continue;
            }
        };

        if !cat_feeder::ds3231::needs_set(&reading, now) {
            continue;
        }
        let drift = reading.trustworthy().map(|held| seconds_between(now, held));

        match rtc.set(now).await {
            Ok(()) => log_rtc_set(now, drift),
            Err(e) => log_rtc_error("could not set it", e),
        }
    }
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_rtc_reading(reading: RtcReading) {
    let t = reading.temperature_q;
    let temp_whole = t.div_euclid(4);
    let temp_frac = t.rem_euclid(4) * 25;
    match reading.wall {
        Some(wall) => info!(
            "rtc: DS3231 holds {wall}, {}, {temp_whole}.{temp_frac:02} C",
            if reading.stopped {
                "oscillator stopped since last set: not trusted"
            } else {
                "running since last set"
            }
        ),
        None => warn!("rtc: DS3231 registers are not a date, {temp_whole}.{temp_frac:02} C"),
    }
    if reading.stops_on_battery {
        warn!("rtc: EOSC is set, so it will stop on the coin cell; cleared on the next set");
    }
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_rtc_error(what: &str, e: cat_feeder::rtc::Error) {
    warn!("rtc: {what} ({e:?})");
}

/// See [`log_start`] for why this is a separate function.
#[inline(never)]
fn log_rtc_set(now: Wall, drift: Option<i64>) {
    match drift {
        None => info!("rtc: set to {now}, it had no trustworthy time"),
        Some(d) => info!("rtc: set to {now}, it was {d}s out"),
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

/// The station stack's sockets: DHCP, MQTT, one spare, and one per admin page
/// connection. Derived, so a change to `http::CONNECTIONS` cannot leave the
/// page's slots failing to `accept`.
type StationSockets = StackResources<{ cat_feeder::http::CONNECTIONS + 3 }>;

#[embassy_executor::task]
async fn web_task(stack: Stack<'static>, store: &'static SharedStore, unit: cat_feeder::web::Unit) {
    cat_feeder::web::run(stack, store, unit, &BUS).await
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}
