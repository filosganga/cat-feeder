//! Setup mode: the unit raises its own network and asks to be configured.
//!
//! Entered from the boot path when there is no usable record in flash, and
//! never left except by rebooting — see [`run`]. The decisions about *what* a
//! record contains and how a form maps onto one live in
//! [`crate::provisioning`], which is pure and host-tested; this module is the
//! radio, the sockets and the HTML.
//!
//! ## The one way in
//!
//! "No usable record" is the only state that reaches here. A reset rewrites
//! the record without its network rather than signalling, so there is no second path and no flag to get
//! out of step. There is deliberately **no fall back into setup after failing
//! to connect**: a router rebooting for five minutes must not drop a working
//! feeder into setup mode and stop it feeding.
//!
//! ## And no timeout
//!
//! A unit stays here until somebody configures it. Rebooting out would only
//! return here, and a unit that gives up while you are fetching your phone is
//! worse than one that waits.
//!
//! ## Built in slices
//!
//! Each slice is verifiable on its own, because the failure modes are
//! completely different: a network that will not appear is a radio problem, a
//! phone that joins and gets no address is DHCP, and a page that will not load
//! is the socket loop.
//!
//! 1. ✅ raise the access point — the SSID appears in a phone's Wi-Fi list
//! 2. ✅ a second network stack on it, DHCP, so a phone gets an address
//! 3. ✅ the form on TCP 80
//! 4. ✅ DNS answers every name with the unit, so a phone opens the form by
//!    itself (ADR-0026) — on Android; iOS not yet tried
//! 5. ✅ access point *and* station: a scan for the SSID box, and a trial —
//!    join, lease, broker login — before anything is saved (ADR-0025). Not
//!    yet run: *Save anyway* (`/keep`)
//! 6. ⬜ the factory reset on the form (`/erase`) — host-tested, not yet run
//!
//! A ✅ means verified on hardware, with a phone.
//!
//! The password is on the console and the sticker and nowhere else on the
//! unit: a unit that has raised a network with a derived password can only say
//! so over a serial cable, which is why `dev/ap-password` exists.

use core::fmt::Write as _;
use core::net::{Ipv4Addr, SocketAddrV4};

use core::str::FromStr as _;

use edge_dhcp::server::{Server, ServerOptions};
use edge_dhcp::{Options, Packet};
use embassy_executor::Spawner;
use embassy_futures::join::{join3, join4};
use embassy_futures::select::{Either3, select3};
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    AccessPointStationEventInfo, AuthenticationMethod, Config as WifiConfig, ControllerConfig,
    DisconnectReason, Interface, WifiController, WifiError,
    ap::AccessPointConfig,
    scan::{ScanConfig, ScanTypeConfig},
    sta::{ScanMethod, StationConfig},
};
use heapless::String;
use log::{error, info, warn};
use static_cell::StaticCell;

use crate::captive_dns;
use crate::config::{AP_SECRET, dhcp_config};
use crate::dhcp::{Mac, SERVER_PORT, Via, reply_to};
use crate::http;
use crate::page::{PAGE_LEN, erase_confirmed};
use crate::provisioning::{
    AP_ADDR_OCTETS, AP_URL, Heard, MAX_NETWORKS, Network, Notice, TESTING_REFRESH_SECS,
    add_network, ap_password, ap_ssid, record_from_form, render_erased, render_form, render_saved,
    render_testing,
};
use crate::record::Record;
use crate::request::Method;
use crate::store::Store;
use crate::trial::{
    BrokerFailure, JoinFailure, Trial, keepable, may_erase, may_start, message, usable_key,
};

/// The address the unit answers on, and the one printed on the console.
///
/// Built from `provisioning::AP_ADDR_OCTETS` rather than written out again,
/// because the pages link to this address too — `provisioning::AP_URL` — and
/// an address that is right on the page and wrong in the socket is a unit that
/// looks configurable and is not. `Ipv4Addr::new` is `const`, which is the
/// whole reason that constant is octets.
const AP_ADDR: Ipv4Addr = Ipv4Addr::new(
    AP_ADDR_OCTETS[0],
    AP_ADDR_OCTETS[1],
    AP_ADDR_OCTETS[2],
    AP_ADDR_OCTETS[3],
);

/// The pool handed out, inclusive.
///
/// Eight addresses for what is nearly always one phone. It is deliberately
/// small: a setup session is one client and a few minutes, and a wide range
/// only makes the lease table bigger for nothing.
const POOL_START: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 2);
const POOL_END: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 9);

/// Lease slots. One per poolable address, so the pool can never outrun the
/// table and start refusing clients it has room for.
const LEASES: usize = 8;

/// How long an address is held.
///
/// Ten minutes, not `edge-dhcp`'s two-hour default. A setup session lasts
/// minutes, and a pool of eight with two-hour leases can be exhausted by a
/// handful of devices wandering past — after which a phone joins, gets nothing,
/// and the only way back is a reboot. At ten minutes the pool recovers on its
/// own. A client renews at half the lease, which costs two packets.
const LEASE_SECS: u32 = 600;

/// Big enough for any DHCP message a client will send. The protocol minimum is
/// 300 bytes and 576 is the usual maximum, so this has room to spare and a
/// truncated request — which `recv_from` reports as an error and nothing else
/// explains — cannot happen.
const DHCP_BUF: usize = 1536;

/// A DNS datagram. 512 bytes is the classic UDP limit, and a phone's query for
/// one name is a fraction of it; an EDNS query that says it *could* take more
/// is still a small packet. The reply is the query plus one 16-byte answer.
const DNS_BUF: usize = 512;

/// How long a trial waits for the Wi-Fi to accept it. An all-channel scan and
/// a WPA2 handshake take a few seconds; a wrong password is reported by the
/// driver well inside this, so reaching it means nothing answered at all.
const JOIN_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a trial waits for a DHCP lease once joined. `dhcp_config` resends
/// DISCOVER every two seconds, so this is several tries.
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to let the station stack see its link go down between trials.
const LINK_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a scan may take. One pass over thirteen channels is a couple of
/// seconds; this only stops a scan that never finishes holding the radio.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest a trial can take, for the testing page to promise: leaving a
/// previous join, joining, a lease, then the broker's TCP connect and CONNACK.
const TRIAL_MAX_SECS: u32 = (LINK_TIMEOUT.as_secs()
    + JOIN_TIMEOUT.as_secs()
    + ADDRESS_TIMEOUT.as_secs()
    + 2 * crate::mqtt::CONNECT_TIMEOUT.as_secs()) as u32;

/// How long a scan listens on each channel. esp-radio's default is 10–20 ms,
/// and on the first phone test a rescan at that, with a phone on the access
/// point, heard 2 access points where the boot scan heard 10. Thirteen
/// channels at the maximum is still about two seconds.
const SCAN_DWELL_MIN: esp_hal::time::Duration = esp_hal::time::Duration::from_millis(60);
const SCAN_DWELL_MAX: esp_hal::time::Duration = esp_hal::time::Duration::from_millis(150);

/// The broker check's socket buffers: a CONNECT, a CONNACK and a DISCONNECT,
/// none more than a few dozen bytes.
const PROBE_BUF: usize = 512;

/// How many DNS answers are logged, so a capture shows the probe arriving
/// without a phone's background chatter filling it.
const DNS_LOGGED: u32 = 8;

/// How often `/rescan` looks for its scan to have finished.
const RESCAN_POLL: Duration = Duration::from_millis(250);

/// How long `/rescan` waits for its scan before showing the form anyway.
const RESCAN_WAIT: Duration = Duration::from_secs(12);

/// How long a unit that saved waits before restarting: three of the testing
/// page's polls, so it shows where the unit went before the network goes.
const REBOOT_DELAY: Duration = Duration::from_secs(3 * TESTING_REFRESH_SECS as u64);

/// What the radio and the form share.
struct Shared {
    trial: Trial,
    /// The form body under trial, or last tried. Re-read for the record to
    /// save, and to fill the form back in when the trial failed. A body is
    /// never longer than the request it came in, so this cannot truncate.
    submitted: String<{ http::REQUEST_LEN }>,
    /// What the last scan heard, for the SSID box.
    networks: heapless::Vec<Network, MAX_NETWORKS>,
    /// Bumped by every scan that finishes, so `/rescan` can tell a new list
    /// from the old one.
    scans: u32,
}

static SHARED: Mutex<CriticalSectionRawMutex, Shared> = Mutex::new(Shared {
    trial: Trial::Idle,
    submitted: String::new(),
    networks: heapless::Vec::new(),
    scans: 0,
});

/// The form asking the radio to try `Shared::submitted`. The radio owns the
/// controller, and this and [`SCAN`] are the only things anything else wants
/// from it.
///
/// Signals rather than a queue: a second request before the first is taken
/// is the same request, and a signal cannot fill up and refuse the one that
/// matters while the page goes on saying *Testing*.
static TRY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The form asking the radio for a fresh list of networks.
static SCAN: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Raises this unit's setup network and stays there.
///
/// Never returns. It reboots once a record is saved, or everything erased, so
/// the normal boot path always starts clean rather than from a
/// half-configured process.
///
/// `id` is the device id: the network's name and password are derived from
/// it, and the broker check logs in under it.
pub async fn run(spawner: Spawner, wifi: WIFI<'static>, id: &str, mut store: Store) -> ! {
    let ssid = ap_ssid(id);
    let ssid = ssid.as_str();
    let password = ap_password(AP_SECRET, id);
    let password = password.as_str();

    // Printed in full, on purpose. A unit in setup mode has few ways to tell
    // anyone its password, and the console is the one that exists before the
    // network does; `dev/ap-password <id>` says it off the device entirely,
    // for stickers.
    info!("setup: raising {ssid}");
    info!("setup: password {password}");
    info!("setup: then browse to http://{AP_ADDR}");

    let ap = AccessPointConfig::default()
        .with_ssid(ssid)
        // **Not optional.** `AccessPointConfig::default()` is an *open*
        // network, so leaving this out would broadcast a setup portal anyone
        // can join — and the session it protects is the one where the home
        // Wi-Fi password gets typed in. The salted password is worthless
        // without it.
        .with_auth_method(AuthenticationMethod::Wpa2Personal)
        .with_password(password.into())
        .with_channel(AP_START_CHANNEL);
    let ap = AccessPoint {
        config: ap,
        channel: AP_START_CHANNEL,
    };

    // Access point *and* station from the start, the station half naming no
    // network and so doing nothing until a trial connects it. Starting in the
    // mode a trial needs is what keeps the phone on: `set_config` stops and
    // restarts Wi-Fi only when the mode changes, and a trial then never does.
    //
    // No separate start call: `set_config` calls `esp_wifi_start()` whenever
    // the mode changes, so applying this as the initial config is what brings
    // the network up.
    let initial = WifiConfig::AccessPointStation(StationConfig::default(), ap.config.clone());
    let (controller, interfaces) = match esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(initial),
    ) {
        Ok(pair) => pair,
        Err(e) => {
            // Nothing useful is left: no network, and no credentials to fall
            // back on. Say so loudly and stop, rather than looping on something
            // that will not start.
            error!("setup: could not raise the access point ({e:?})");
            halt().await
        }
    };

    info!("setup: access point up");

    let ap_stack = start_ap_stack(spawner, interfaces.access_point);
    let station = start_station_stack(spawner, interfaces.station);

    // What the unit knew before, if anything decoded. It is *not* usable — that
    // is why we are here — but it may still carry this unit's bench-measured
    // detent interval and portion scale, which the form does not ask for and
    // must not silently reset to defaults. See `record_from_form`'s `previous`.
    let previous = previous_record(&mut store);
    if previous.is_some() {
        info!("setup: keeping this unit's measured timings from the old record");
    }

    // Shared because the radio saves a trial that passed, and the connection
    // slots save one kept by *Save anyway*. Contended only in the moment one
    // of them writes, and that moment ends in a reboot.
    let store = Mutex::<CriticalSectionRawMutex, _>::new(store);

    // `.0` is `!`: every branch diverges, so the tuple is uninhabited and this
    // expression is the function's divergence rather than a value.
    join4(
        serve_dhcp(ap_stack),
        serve_dns(ap_stack),
        radio(controller, ap, station, &store, previous.as_ref(), id),
        serve_form(ap_stack, &store, previous.as_ref(), ssid),
    )
    .await
    .0
}

/// The old record, if flash holds one that decodes at all.
///
/// Its own function for the same reason as `main.rs`'s `stored_config`: a
/// `Record` is a few hundred bytes, the moves in and out of one are not elided
/// at this optimisation level, and this must not land in the async frame that
/// lives for the whole of setup mode.
#[inline(never)]
fn previous_record(store: &mut Store) -> Option<Record> {
    store.load().ok()
}

/// Where the access point starts, before the boot scan says where the networks
/// are: esp-radio's own default, written down so `follow` can know it.
const AP_START_CHANNEL: u8 = 1;

/// The access point's config and the channel in it, which esp-radio does not
/// let anything read back.
struct AccessPoint {
    config: AccessPointConfig,
    channel: u8,
}

/// Owns the controller: scans, trials, and the console's record of who joined.
///
/// One owner because a scan and a connect need the controller `&mut`, and the
/// station events need it `&` — they cannot be three futures at once. The
/// event wait is the idle branch of a `select`, dropped whenever a command
/// arrives and taken again afterwards.
async fn radio(
    mut controller: WifiController<'static>,
    mut ap: AccessPoint,
    station: Stack<'static>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
    id: &str,
) -> ! {
    static PROBE_RX: StaticCell<[u8; PROBE_BUF]> = StaticCell::new();
    static PROBE_TX: StaticCell<[u8; PROBE_BUF]> = StaticCell::new();
    let rx = PROBE_RX.init([0; PROBE_BUF]);
    let tx = PROBE_TX.init([0; PROBE_BUF]);

    // Before any phone has joined, ideally: a scan takes the radio off the
    // access point's channel for a moment.
    scan(&mut controller).await;

    // Then move the access point to where the strongest network is — most
    // likely the one it will be given — while there is still nobody on it to
    // drop. See `follow`.
    let strongest = SHARED.lock().await.networks.first().map(|n| n.channel);
    if let Some(channel) = strongest {
        follow(&mut controller, &mut ap, channel, "the strongest network");
    }

    loop {
        // A `let`, not a `match` on the expression: temporaries in a match
        // scrutinee live to the end of the match, and the event future's
        // borrow of the controller would outlast the arms that need it `&mut`.
        //
        // A trial is listed first, so it wins when both are waiting.
        let next = select3(
            TRY.wait(),
            SCAN.wait(),
            controller.wait_for_access_point_connected_event_async(),
        )
        .await;

        match next {
            Either3::Second(()) => scan(&mut controller).await,
            Either3::First(()) => {
                let outcome = attempt(
                    &mut controller,
                    &mut ap,
                    station,
                    store,
                    previous,
                    id,
                    rx,
                    tx,
                )
                .await;
                SHARED.lock().await.trial = outcome;
                if let Trial::Saved { .. } = outcome {
                    Timer::after(REBOOT_DELAY).await;
                    info!("setup: saved, restarting");
                    esp_hal::system::software_reset()
                }
            }
            Either3::Third(event) => log_station(event),
        }
    }
}

/// Says on the console who has joined the setup network.
///
/// Nothing else needs this — the DHCP server neither knows nor cares whether a
/// station associated. It is here purely so the console can tell two very
/// different failures apart, which it could not before:
///
/// ```text
/// setup: station 12:34:.. associated     <- no dhcp line after it: DHCP is broken
/// (nothing at all)                       <- the phone never joined: radio, SSID or password
/// ```
///
/// **Read it as evidence, not proof.** The wait takes a fresh subscription
/// every time, and is not running at all while the radio scans or tries a
/// network, so an event in those gaps is dropped. A missing association line
/// is therefore *probably* a phone that never joined, and a DHCP line without
/// one above it is not a contradiction. The DHCP lines are the ones that
/// cannot be missed, because they come from the socket itself.
fn log_station(event: Result<AccessPointStationEventInfo, WifiError>) {
    match event {
        Ok(AccessPointStationEventInfo::Connected(info)) => {
            info!("setup: station {} associated", Mac(info.mac));
        }
        Ok(AccessPointStationEventInfo::Disconnected(info)) => {
            info!("setup: station {} left ({:?})", Mac(info.mac), info.reason);
        }
        Err(e) => warn!("setup: station event failed ({e:?})"),
    }
}

/// Lists the networks in range, for the form.
///
/// Longer per channel than esp-radio's default — see [`SCAN_DWELL_MIN`].
///
/// **A rescan adds to the list rather than replacing it**, so a scan that
/// hears less cannot empty a list that was right. Each name keeps its
/// strongest signal; nothing drops out within one setup session, which lasts
/// minutes.
async fn scan(controller: &mut WifiController<'static>) {
    let config = ScanConfig::default().with_scan_type(ScanTypeConfig::Active {
        min: SCAN_DWELL_MIN,
        max: SCAN_DWELL_MAX,
    });
    let found = match with_timeout(SCAN_TIMEOUT, controller.scan_async(&config)).await {
        Ok(Ok(found)) => found,
        Ok(Err(e)) => {
            warn!("setup: scan failed ({e:?})");
            return;
        }
        Err(_) => {
            warn!("setup: scan did not finish in {}s", SCAN_TIMEOUT.as_secs());
            return;
        }
    };

    let mut shared = SHARED.lock().await;
    for ap in &found {
        add_network(
            &mut shared.networks,
            Heard {
                ssid: ap.ssid.as_str(),
                rssi: ap.signal_strength,
                channel: ap.channel,
                bssid: ap.bssid,
            },
        );
    }
    shared.scans = shared.scans.wrapping_add(1);
    info!(
        "setup: scan heard {} access points, {} networks",
        found.len(),
        shared.networks.len()
    );
}

/// Tries the submitted settings: join, address, broker. Saves them only if the
/// Wi-Fi joined and the broker took the login (ADR-0025).
///
/// The Wi-Fi is tried on the station half while the access point stays up.
/// Once it joins, the access point moves to the router's channel, which a
/// phone may notice as a moment off the network — the reason the testing page
/// polls rather than waits.
#[allow(clippy::too_many_arguments, reason = "one call site, all of it wiring")]
async fn attempt(
    controller: &mut WifiController<'static>,
    ap: &mut AccessPoint,
    station: Stack<'static>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
    id: &str,
    rx: &mut [u8],
    tx: &mut [u8],
) -> Trial {
    // Validated when it was submitted, so this does not fail; if it somehow
    // did, there is nothing to try.
    let Some(record) = submitted_record(previous).await else {
        return Trial::WifiFailed(JoinFailure::Other);
    };
    info!(
        "setup: trying {} then {}:{}",
        record.wifi_ssid, record.mqtt_host, record.mqtt_port
    );

    if !usable_key(&record.wifi_password) {
        info!(
            "setup: a {}-character key cannot be a WPA2 key",
            record.wifi_password.len()
        );
        return Trial::WifiFailed(JoinFailure::UnusableKey);
    }

    // A previous trial that joined and then failed at the broker is still
    // connected. Leave, and let the station stack see the link drop: it only
    // forgets its lease on a link change it has polled, and a stale lease
    // would read as this trial's address.
    if controller.is_connected() {
        let _ = controller.disconnect_async().await;
        let _ = with_timeout(LINK_TIMEOUT, station.wait_link_down()).await;
    }

    let candidate = StationConfig::default()
        .with_ssid(record.wifi_ssid.as_str())
        .with_password(record.wifi_password.as_str().into())
        // The same as the station mode it is rehearsing: the strongest node
        // of a mesh, not the first heard.
        .with_scan_method(ScanMethod::AllChannels);
    // **Straight to the node the scan heard best, if it heard one.** Searching
    // every channel takes the access point off its own for each, and on the
    // first phone test that cost the phone the setup network for the whole of
    // a 20 s trial. A network the scan never heard — a hidden one — is still
    // searched for.
    let heard = SHARED
        .lock()
        .await
        .networks
        .iter()
        .find(|n| n.ssid == record.wifi_ssid)
        .map(|n| (n.channel, n.bssid));
    let candidate = match heard {
        Some((channel, bssid)) => {
            info!("setup: {} was heard on channel {channel}", record.wifi_ssid);
            follow(controller, ap, channel, &record.wifi_ssid);
            candidate.with_channel(channel).with_bssid(bssid)
        }
        None => {
            info!(
                "setup: {} was not heard; searching every channel",
                record.wifi_ssid
            );
            candidate
        }
    };
    if let Err(e) = controller.set_config(&WifiConfig::AccessPointStation(
        candidate,
        ap.config.clone(),
    )) {
        // `set_config` stops Wi-Fi when it fails, access point included. Put
        // the access point back, or the phone has nothing to show this on.
        error!("setup: the driver refused that network ({e:?}); raising the access point again");
        let restore = WifiConfig::AccessPointStation(StationConfig::default(), ap.config.clone());
        if let Err(e) = controller.set_config(&restore) {
            error!("setup: could not raise the access point again ({e:?})");
        }
        return Trial::WifiFailed(JoinFailure::Other);
    }

    match with_timeout(JOIN_TIMEOUT, controller.connect_async()).await {
        Ok(Ok(info)) => info!(
            "setup: joined {} on channel {}",
            record.wifi_ssid, info.channel
        ),
        Ok(Err(WifiError::Disconnected(info))) => {
            info!("setup: {} refused ({:?})", record.wifi_ssid, info.reason);
            return Trial::WifiFailed(join_failure(info.reason));
        }
        Ok(Err(e)) => {
            warn!("setup: could not join {} ({e:?})", record.wifi_ssid);
            return Trial::WifiFailed(JoinFailure::Other);
        }
        Err(_) => {
            info!(
                "setup: {} did not answer in {}s",
                record.wifi_ssid,
                JOIN_TIMEOUT.as_secs()
            );
            return Trial::WifiFailed(JoinFailure::Other);
        }
    }

    if with_timeout(ADDRESS_TIMEOUT, station.wait_config_up())
        .await
        .is_err()
    {
        info!("setup: joined, no lease in {}s", ADDRESS_TIMEOUT.as_secs());
        return Trial::WifiFailed(JoinFailure::NoAddress);
    }
    let Some(v4) = station.config_v4() else {
        return Trial::WifiFailed(JoinFailure::NoAddress);
    };
    let addr = v4.address.address().octets();
    info!("setup: given {}", v4.address.address());

    // Validated as an address by the form.
    let Ok(host) = Ipv4Addr::from_str(&record.mqtt_host) else {
        return Trial::BrokerFailed {
            addr,
            why: BrokerFailure::Unreachable,
        };
    };
    if let Err(why) = crate::mqtt::probe(
        station,
        host,
        record.mqtt_port,
        &record.mqtt_user,
        &record.mqtt_password,
        id,
        rx,
        tx,
    )
    .await
    {
        info!("setup: the broker check failed ({why:?}); nothing saved");
        return Trial::BrokerFailed { addr, why };
    }

    match store.lock().await.save(&record) {
        Ok(()) => {
            info!(
                "setup: saved {} via {}:{}",
                record.wifi_ssid, record.mqtt_host, record.mqtt_port
            );
            Trial::Saved { addr }
        }
        Err(e) => {
            error!("setup: could not save ({e:?})");
            Trial::NotSaved
        }
    }
}

/// Puts the access point on `channel`, if it is not there already.
///
/// **Every trial re-applies the access point's config.** esp-radio has no way
/// to set the station half alone: `set_config` in this mode writes both, the
/// access point's channel included, and an access point whose config changes
/// restarts and drops its clients. A station joining on another channel drags
/// the access point there too. On the second phone test both cost the phone
/// the setup network on every trial.
///
/// So the access point's channel is kept equal to the network's: once after
/// the boot scan, and again by a trial on a network elsewhere. That keeps a
/// trial from also dragging the access point across channels — but on the
/// third phone test the phone still dropped on every trial with the channel
/// already right, most likely because re-applying even an unchanged config
/// restarts the access point. The testing page warns of it; the phone came
/// back by itself within seconds, and `/` has the outcome when it does.
/// Setting the station half alone would need esp-radio's private bindings.
fn follow(controller: &mut WifiController<'static>, ap: &mut AccessPoint, channel: u8, why: &str) {
    if ap.channel == channel {
        return;
    }
    ap.config = ap.config.clone().with_channel(channel);
    ap.channel = channel;
    let config = WifiConfig::AccessPointStation(StationConfig::default(), ap.config.clone());
    match controller.set_config(&config) {
        Ok(()) => info!("setup: access point moved to channel {channel}, for {why}"),
        Err(e) => error!("setup: could not move the access point to channel {channel} ({e:?})"),
    }
}

/// The record under trial, re-read from the body that was submitted.
async fn submitted_record(previous: Option<&Record>) -> Option<Record> {
    record_from_form(&SHARED.lock().await.submitted, previous).ok()
}

/// Sixty-odd disconnect reasons, as the four a person can act on. The reason
/// itself is on the console.
fn join_failure(reason: DisconnectReason) -> JoinFailure {
    match reason {
        DisconnectReason::FourWayHandshakeTimeout
        | DisconnectReason::HandshakeTimeout
        | DisconnectReason::AuthenticationFailed => JoinFailure::WrongPassword,
        DisconnectReason::NoAccessPointFound
        | DisconnectReason::NoAccessPointFoundWithCompatibleSecurity
        | DisconnectReason::NoAccessPointFoundInAuthmodeThreshold
        | DisconnectReason::NoAccessPointFoundInRssiThreshold => JoinFailure::NotFound,
        _ => JoinFailure::Other,
    }
}

/// Brings up the setup network's own embassy-net stack.
///
/// This is not the station-mode stack in `main.rs`, which is not reused and
/// could not be: this is a different interface with a different address, and
/// the two never exist at once anyway, because setup mode is entered before
/// any of the normal tasks are spawned.
///
/// Static, not DHCP, because on this network the unit *is* the server.
fn start_ap_stack(spawner: Spawner, device: Interface<'static>) -> Stack<'static> {
    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(AP_ADDR, 24),
        // No route off this network, because there is nowhere to go: the unit
        // forwards nothing. What the *clients* are told is a separate
        // question — see `ServerOptions` in `serve_dhcp`.
        gateway: None,
        dns_servers: heapless::Vec::new(),
    });

    // Two UDP sockets, DHCP and DNS, plus `CONNECTIONS` TCP ones for the form,
    // and a spare. Too few shows up as `accept failed (InvalidState)` rather
    // than as anything about sockets.
    static RESOURCES: StaticCell<StackResources<{ http::CONNECTIONS + 3 }>> = StaticCell::new();

    let (stack, runner) = embassy_net::new(
        device,
        config,
        RESOURCES.init(StackResources::new()),
        seed(),
    );
    spawner.spawn(net_task(runner).expect("failed to create setup net task"));
    stack
}

/// The station half's stack, for trials: a DHCP client, as `main.rs`'s is, and
/// the broker check's one TCP socket.
fn start_station_stack(spawner: Spawner, device: Interface<'static>) -> Stack<'static> {
    // DHCP and the broker check, and a spare.
    const SOCKETS: usize = 3;
    static RESOURCES: StaticCell<StackResources<SOCKETS>> = StaticCell::new();

    let (stack, runner) = embassy_net::new(
        device,
        embassy_net::Config::dhcpv4(dhcp_config()),
        RESOURCES.init(StackResources::new()),
        seed(),
    );
    spawner.spawn(net_task(runner).expect("failed to create trial net task"));
    stack
}

fn seed() -> u64 {
    let rng = Rng::new();
    ((rng.random() as u64) << 32) | rng.random() as u64
}

/// Polls a setup stack: the access point's, and the station's for trials.
///
/// A near-twin of `main.rs`'s `net_task`, and separate for a reason that is not
/// style: that one is defined in the binary crate, and a library module cannot
/// spawn a task it cannot name. The duplication is two lines.
#[embassy_executor::task(pool_size = 2)]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}

/// Hands out addresses, forever.
///
/// `edge-dhcp` is a codec rather than a server: [`Server::handle_request`]
/// takes a decoded [`Packet`] and gives back the one to send, and moving the
/// bytes is this loop's job. That is why the crate is pulled in with
/// `default-features = false` — its `io` module would drag in `edge-nal` and a
/// second socket abstraction over the stack that is already here.
///
/// The reply destination follows RFC 2131 §4.1, and it is the part worth
/// getting right: a client being offered its first address has no address yet,
/// so the reply cannot be unicast to it. Broadcast is the default and the only
/// safe answer until the client tells us a `ciaddr` it is already reachable on.
async fn serve_dhcp(stack: Stack<'static>) -> ! {
    // Static rather than local. This runs on `main`'s task stack, and three
    // buffers of this size on it is exactly the kind of overflow that shows up
    // as an unexplained reboot rather than as an error.
    static RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
    static TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; DHCP_BUF]> = StaticCell::new();
    static TX_BUF: StaticCell<[u8; DHCP_BUF]> = StaticCell::new();
    static SCRATCH: StaticCell<[u8; DHCP_BUF]> = StaticCell::new();

    let mut socket = UdpSocket::new(
        stack,
        RX_META.init([PacketMetadata::EMPTY; 4]),
        RX_BUF.init([0; DHCP_BUF]),
        TX_META.init([PacketMetadata::EMPTY; 4]),
        TX_BUF.init([0; DHCP_BUF]),
    );
    let buf = SCRATCH.init([0; DHCP_BUF]);

    // Bound to the port on every address, which is what lets the socket see
    // the broadcast a client with no address has to send.
    if let Err(e) = socket.bind(SERVER_PORT) {
        error!("setup: could not bind udp/{SERVER_PORT} ({e:?})");
        halt().await
    }

    let mut server: Server<_, LEASES> = Server::new(|| Instant::now().as_secs(), AP_ADDR);
    // `Server::new` defaults to .50–.200, which is not this pool.
    server.range_start = POOL_START;
    server.range_end = POOL_END;

    // The unit advertises *itself* as the client's gateway even though it
    // routes nothing. That is what every ESP-IDF softAP does, and it is the
    // safer of the two: a phone handed no router at all can decide the network
    // is broken and drop it, whereas one that routes to us simply finds its
    // packets go nowhere — which is true, and the point.
    //
    // The unit is the DNS server too, and every name it is asked resolves to
    // itself — see `serve_dns` and ADR-0026. Not option 114 (RFC 8910): that
    // names an RFC 8908 *API*, expected over HTTPS and answering JSON, and a
    // phone handed this form's plain-HTTP address there could well distrust
    // the network rather than open it.
    let mut gateway = [AP_ADDR];
    let mut options = ServerOptions::new(AP_ADDR, Some(&mut gateway));
    options.dns = &[AP_ADDR];
    options.lease_duration_secs = LEASE_SECS;

    info!("setup: dhcp on {AP_ADDR}, pool {POOL_START}-{POOL_END}");

    loop {
        let len = match socket.recv_from(buf).await {
            Ok((len, _from)) => len,
            Err(e) => {
                warn!("setup: dhcp recv failed ({e:?})");
                continue;
            }
        };

        let request = match Packet::decode(&buf[..len]) {
            Ok(request) => request,
            Err(e) => {
                warn!("setup: dhcp packet ignored ({e:?})");
                continue;
            }
        };

        let mac = Mac::from_chaddr(request.chaddr);

        let mut opt_buf = Options::buf();
        let Some(reply) = server.handle_request(&mut opt_buf, &options, &request) else {
            // Not only a release or a decline, which are the silent ones. This
            // is also what an exhausted pool looks like, and a station sitting
            // here with no address would otherwise show on the console as an
            // association and then nothing — which `log_station` above
            // teaches the reader to interpret as a broken socket loop. Say
            // which it is.
            warn!("setup: dhcp had no answer for {mac} (pool full, or not a request for us)");
            continue;
        };

        // A NAK carries no address: `ack_nak` leaves `yiaddr` unspecified when
        // it refuses. Worth distinguishing, because a phone remembering a
        // 192.168.4.x lease from some *other* ESP access point — they all use
        // this subnet — renews it against us and is refused, and logging that
        // as an address handed out is how the bench would be told a lie.
        let refused = reply.yiaddr.is_unspecified();
        if refused {
            warn!("setup: dhcp refused {mac} (it asked for an address outside the pool)");
        } else {
            // The same spelling as the association line above, so the two can
            // be read against each other.
            info!("setup: dhcp {} -> {mac}", reply.yiaddr);
        }

        let (dest_ip, dest_port) = reply_to(Via {
            giaddr: request.giaddr,
            ciaddr: request.ciaddr,
            broadcast: request.broadcast,
            nak: refused,
        });

        // `request` is finished with, so the shared borrow of `buf` has ended
        // and the reply can be encoded into the same buffer.
        let encoded = match reply.encode(buf) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("setup: dhcp reply too large ({e:?})");
                continue;
            }
        };

        if let Err(e) = socket
            .send_to(encoded, SocketAddrV4::new(dest_ip, dest_port))
            .await
        {
            warn!("setup: dhcp send failed ({e:?})");
        }
    }
}

/// Answers every name with this unit, forever.
///
/// This is what makes the phone open the form by itself: its connectivity
/// probe resolves here, gets the form instead of `204` or `Success`, and the
/// phone shows it as a captive portal. The packet logic, and which queries go
/// unanswered, is [`captive_dns::answer`].
///
/// One line per name rather than per query: a phone asks the same few names
/// over and over, and only the first says anything.
async fn serve_dns(stack: Stack<'static>) -> ! {
    // Static for the same reason as `serve_dhcp`'s: `main`'s task stack.
    static RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
    static TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; DNS_BUF]> = StaticCell::new();
    static TX_BUF: StaticCell<[u8; DNS_BUF]> = StaticCell::new();
    static QUERY: StaticCell<[u8; DNS_BUF]> = StaticCell::new();
    static REPLY: StaticCell<[u8; DNS_BUF]> = StaticCell::new();

    let mut socket = UdpSocket::new(
        stack,
        RX_META.init([PacketMetadata::EMPTY; 4]),
        RX_BUF.init([0; DNS_BUF]),
        TX_META.init([PacketMetadata::EMPTY; 4]),
        TX_BUF.init([0; DNS_BUF]),
    );
    let query = QUERY.init([0; DNS_BUF]);
    let reply = REPLY.init([0; DNS_BUF]);

    if let Err(e) = socket.bind(captive_dns::PORT) {
        error!("setup: could not bind udp/{} ({e:?})", captive_dns::PORT);
        halt().await
    }

    info!("setup: dns on {AP_ADDR}, every name is {AP_ADDR}");

    let mut answered = 0u32;
    loop {
        let (len, from) = match socket.recv_from(query).await {
            Ok(pair) => pair,
            Err(e) => {
                warn!("setup: dns recv failed ({e:?})");
                continue;
            }
        };

        let Some(n) = captive_dns::answer(&query[..len], AP_ADDR_OCTETS, reply) else {
            info!(
                "setup: dns ignored a {len} byte packet from {}",
                from.endpoint
            );
            continue;
        };

        if answered < DNS_LOGGED {
            info!("setup: dns answered {} ({len} bytes)", from.endpoint);
        }
        answered = answered.saturating_add(1);

        if let Err(e) = socket.send_to(&reply[..n], from).await {
            warn!("setup: dns send failed ({e:?})");
        }
    }
}

/// Stops, without spinning.
async fn halt() -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}

// ---------------------------------------------------------------------------
// The form
// ---------------------------------------------------------------------------

/// Serves the form until a record is saved, then reboots.
///
/// **Several connections at once, not one.** A browser opens more than it uses,
/// and sends nothing on some of them. With a single socket the next real
/// request is refused with a RST while an idle one is still being waited on —
/// which is what "this site can't be reached" looked like when Save was
/// pressed, moments after the GET that drew the form had worked perfectly.
async fn serve_form(
    stack: Stack<'static>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
    setup_ssid: &str,
) -> ! {
    let [s0, s1, s2] = http::slots();

    info!(
        "setup: form on http://{AP_ADDR}/, {} connections",
        http::CONNECTIONS
    );

    join3(
        connection(0, stack, s0, store, previous, setup_ssid),
        connection(1, stack, s1, store, previous, setup_ssid),
        connection(2, stack, s2, store, previous, setup_ssid),
    )
    .await
    .0
}

/// One connection slot: accept, answer, recycle, forever.
async fn connection(
    slot: u8,
    stack: Stack<'static>,
    buffers: http::Slot,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
    setup_ssid: &str,
) -> ! {
    let http::Slot {
        rx,
        tx,
        request,
        page,
    } = buffers;
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(http::TIMEOUT));

    loop {
        if let Err(e) = socket.accept(http::PORT).await {
            warn!("setup: [{slot}] accept failed ({e:?})");
            http::reset(&mut socket).await;
            continue;
        }

        // *Save anyway* leaves setup mode from here; a trial that passes
        // restarts from the radio, and a factory reset from `erase`.
        if let Outcome::Restart = handle(
            slot,
            &mut socket,
            request,
            page,
            store,
            previous,
            setup_ssid,
        )
        .await
        {
            // The browser is told before the unit disappears. Without the
            // flush the reset races the last segment and the phone shows a
            // connection error on what was actually a success.
            socket.close();
            let _ = socket.flush().await;
            Timer::after(Duration::from_millis(250)).await;

            info!("setup: restarting");
            esp_hal::system::software_reset()
        }

        http::reset(&mut socket).await;
    }
}

enum Outcome {
    /// A record is in flash. Reboot into it.
    Restart,
    /// Anything else: the connection is finished with, setup mode continues.
    Continue,
}

/// Reads one request and answers it.
///
/// | | |
/// |---|---|
/// | `GET /` | the last trial's outcome, or the form with what the scan heard |
/// | `POST /save` | starts a trial, then `303` to `/status` |
/// | `GET /status` | testing, the form again with what failed, or saved |
/// | `POST /keep` | *Save anyway*: the tested record, broker unchecked |
/// | `GET /rescan` | scans, then `302` to `/` |
/// | `POST /erase` | the factory reset: everything erased, then a restart |
/// | anything else | `302` to `/` — a phone's connectivity probe, mostly |
async fn handle(
    slot: u8,
    socket: &mut TcpSocket<'_>,
    request: &mut [u8; http::REQUEST_LEN],
    page: &mut String<PAGE_LEN>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
    setup_ssid: &str,
) -> Outcome {
    let Some((head, body)) = http::read_request("setup", slot, socket, request).await else {
        return Outcome::Continue;
    };

    // Every request, before it is acted on. This is the line whose absence made
    // a working GET and a refused POST look identical on the console, and it is
    // cheap: a phone joining produces a handful of these, not a stream.
    info!(
        "setup: [{slot}] {:?} {} ({} byte body)",
        head.method, head.path, head.content_length
    );

    match (head.method, head.path.as_str()) {
        (Method::Post, "/save") => {
            if let Err(e) = record_from_form(body, previous) {
                // Not an error on the console: a typo is ordinary, and
                // warning about every one would make a real fault harder
                // to spot in a capture.
                info!("setup: form rejected ({e:?})");
                render_form(
                    page,
                    body,
                    Notice::Invalid(e),
                    &SHARED.lock().await.networks,
                );
                // 200, not 400. The body *is* the answer — the form again,
                // with the message — and a browser shows it either way.
                http::send("setup", socket, "200 OK", "", page).await;
                return Outcome::Continue;
            }

            {
                let mut shared = SHARED.lock().await;
                // A second press while one is running, or after one saved,
                // starts nothing: the status page already says how it went.
                if may_start(shared.trial) {
                    shared.submitted.clear();
                    let _ = shared.submitted.push_str(body);
                    shared.trial = Trial::Testing;
                    TRY.signal(());
                }
            }
            // See Other, so a reload asks for the status rather than posting
            // the form a second time.
            redirect(socket, page, "303 See Other", "/status").await;
            Outcome::Continue
        }
        (Method::Get, "/status") => {
            let rendered = render_status(page, &*SHARED.lock().await, previous, setup_ssid);
            if rendered {
                http::send("setup", socket, "200 OK", "", page).await;
            } else {
                redirect(socket, page, "302 Found", "/").await;
            }
            Outcome::Continue
        }
        (Method::Post, "/keep") => keep(socket, page, store, previous).await,
        (Method::Post, "/erase") => erase(socket, page, store, body, setup_ssid).await,
        (Method::Get, "/rescan") => {
            let before = SHARED.lock().await.scans;
            SCAN.signal(());
            let _ = with_timeout(RESCAN_WAIT, async {
                while SHARED.lock().await.scans == before {
                    Timer::after(RESCAN_POLL).await;
                }
            })
            .await;
            redirect(socket, page, "302 Found", "/").await;
            Outcome::Continue
        }
        (Method::Other, _) => {
            page.clear();
            let _ = page.push_str("Method not allowed.");
            http::send("setup", socket, "405 Method Not Allowed", "", page).await;
            Outcome::Continue
        }
        // The last trial's outcome if there was one, not a blank form. A phone
        // that lost the setup network mid-trial closes the portal, and the
        // page it opens on rejoining is this one; the answer must be here.
        (Method::Get, "/") => {
            {
                let shared = SHARED.lock().await;
                if !render_status(page, &shared, previous, setup_ssid) {
                    render_form(page, "", Notice::None, &shared.networks);
                }
            }
            http::send("setup", socket, "200 OK", "", page).await;
            Outcome::Continue
        }
        // Everything else, which is mostly a phone's connectivity probe —
        // `/generate_204`, `/hotspot-detect.html` — arriving here because
        // `serve_dns` sent it. A redirect rather than the form itself, so the
        // portal sheet shows the unit's address and the form posts back to it,
        // not to `connectivitycheck.gstatic.com`.
        _ => {
            redirect(socket, page, "302 Found", "/").await;
            Outcome::Continue
        }
    }
}

/// The status page for where the trial is. `false` when there is nothing to
/// show, because nothing was submitted.
fn render_status(
    page: &mut String<PAGE_LEN>,
    shared: &Shared,
    previous: Option<&Record>,
    setup_ssid: &str,
) -> bool {
    if shared.trial == Trial::Idle {
        return false;
    }
    let Ok(record) = record_from_form(&shared.submitted, previous) else {
        return false;
    };
    match shared.trial {
        Trial::Idle => return false,
        Trial::Testing => render_testing(page, &record.wifi_ssid, setup_ssid, TRIAL_MAX_SECS),
        Trial::Saved { addr } => render_saved(page, &record, addr),
        failed => {
            let Some(html) = message(
                failed,
                &record.wifi_ssid,
                &record.mqtt_host,
                record.mqtt_port,
            ) else {
                return false;
            };
            let notice = Notice::Trial {
                html: &html,
                offer_keep: matches!(failed, Trial::BrokerFailed { .. }),
            };
            render_form(page, &shared.submitted, notice, &shared.networks);
        }
    }
    true
}

/// *Save anyway*: the record whose Wi-Fi joined and whose broker did not.
///
/// Only from that state. The record is the tested one, re-read from what was
/// submitted, never the boxes as they are now, which may have been edited
/// since.
async fn keep(
    socket: &mut TcpSocket<'_>,
    page: &mut String<PAGE_LEN>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
) -> Outcome {
    let kept = {
        let shared = SHARED.lock().await;
        keepable(shared.trial).and_then(|addr| {
            record_from_form(&shared.submitted, previous)
                .ok()
                .map(|record| (record, addr))
        })
    };
    let Some((record, addr)) = kept else {
        redirect(socket, page, "303 See Other", "/status").await;
        return Outcome::Continue;
    };

    match store.lock().await.save(&record) {
        Ok(()) => {
            info!(
                "setup: saved {} via {}:{}, broker unchecked",
                record.wifi_ssid, record.mqtt_host, record.mqtt_port
            );
            render_saved(page, &record, addr);
            http::send("setup", socket, "200 OK", "", page).await;
            Outcome::Restart
        }
        Err(e) => {
            // Flash refused the write. Say so on the page rather than
            // rebooting into a unit that is still unconfigured and cannot
            // explain why.
            error!("setup: could not save ({e:?})");
            SHARED.lock().await.trial = Trial::NotSaved;
            redirect(socket, page, "303 See Other", "/status").await;
            Outcome::Continue
        }
    }
}

/// The factory reset: credentials, calibration, meals, timezone and pause.
///
/// **A restart follows, always.** Setup read the old record once, at the
/// start, to carry its calibration into whatever is saved (`previous`), so
/// carrying on would put the erased calibration back at the next save. After
/// the restart there is nothing to carry. Refused while a trial runs or has
/// saved, for the same reason: a trial writes `previous`'s calibration
/// (`trial::may_erase`).
///
/// The store stays locked from the erase to the restart, so a trial started
/// in between cannot save into it.
async fn erase(
    socket: &mut TcpSocket<'_>,
    page: &mut String<PAGE_LEN>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    body: &str,
    setup_ssid: &str,
) -> Outcome {
    let confirmed = erase_confirmed(body);
    if !may_erase(confirmed, SHARED.lock().await.trial) {
        info!(
            "setup: factory reset refused ({})",
            if confirmed {
                "a trial is running"
            } else {
                "the box was not ticked"
            }
        );
        redirect(socket, page, "303 See Other", "/").await;
        return Outcome::Continue;
    }
    let mut store = store.lock().await;
    if let Err(e) = store.erase_all() {
        error!("setup: factory reset failed ({e:?})");
        drop(store);
        render_form(
            page,
            "",
            Notice::Failed("Erasing failed; nothing changed."),
            &SHARED.lock().await.networks,
        );
        http::send("setup", socket, "200 OK", "", page).await;
        return Outcome::Continue;
    }
    warn!("setup: factory reset, everything erased");
    render_erased(page, setup_ssid);
    http::send("setup", socket, "200 OK", "", page).await;
    // As `connection` restarts, but with the store still locked.
    socket.close();
    let _ = socket.flush().await;
    Timer::after(Duration::from_millis(250)).await;
    info!("setup: restarting");
    esp_hal::system::software_reset()
}

/// An empty response sending the browser to `path` on the unit's address.
async fn redirect(
    socket: &mut TcpSocket<'_>,
    page: &mut String<PAGE_LEN>,
    status: &str,
    path: &str,
) {
    page.clear();
    let mut location: String<64> = String::new();
    let _ = write!(location, "Location: {AP_URL}{path}\r\n");
    http::send("setup", socket, status, &location, page).await;
}
