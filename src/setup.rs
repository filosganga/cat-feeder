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
//! "No usable record" is the only state that reaches here. The reset button
//! erases rather than signalling, so there is no second path and no flag to get
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
//! 3. ✅ the form on TCP 80, and a saved record reboots into normal mode
//! 4. ⬜ the panel says what to join and what to type into it — written and
//!    host-tested, **not yet seen on glass**
//!
//! The first three ✅ mean verified on hardware, with a phone. The fourth ⬜
//! does not mean unbuilt: it is wired and its layout is host-tested, and what
//! is missing is a capture, because seeing it needs a unit with no record and
//! therefore a button held through power-on.
//!
//! It is also not a network slice at all, which is why it is listed last and
//! owned by `main.rs`: the boot path spawns `display_task` before calling
//! [`run`], because this function never returns and could not spawn it
//! afterwards. It is the one screen whose contents exist nowhere else — a unit
//! that has raised a network with a derived password can otherwise only say so
//! over a serial cable.

use core::fmt::Write as _;
use core::net::{Ipv4Addr, SocketAddrV4};

use edge_dhcp::server::{Server, ServerOptions};
use edge_dhcp::{Options, Packet};
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write as _;
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    AccessPointStationEventInfo, AuthenticationMethod, Config as WifiConfig, ControllerConfig,
    Interface, WifiController, ap::AccessPointConfig,
};
use heapless::String;
use log::{error, info, warn};
use static_cell::StaticCell;

use crate::dhcp::{Mac, SERVER_PORT, Via, reply_to};
use crate::provisioning::{
    AP_ADDR_OCTETS, Head, Method, PAGE_LEN, Record, parse_head, record_from_form, render_form,
    render_saved,
};
use crate::store::Store;

/// The address the unit answers on, and the one printed on the console.
///
/// Built from `provisioning::AP_ADDR_OCTETS` rather than written out again,
/// because the panel shows this address too — see `display::render` — and an
/// address that is right on the screen and wrong in the socket is a unit that
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

/// The form's port. Plain HTTP: there is no certificate a self-signed unit
/// could present that a phone would not shout about, and the link itself is
/// WPA2 — which is the reason the AP password is salted.
const HTTP_PORT: u16 = 80;

/// Big enough for any DHCP message a client will send. The protocol minimum is
/// 300 bytes and 576 is the usual maximum, so this has room to spare and a
/// truncated request — which `recv_from` reports as an error and nothing else
/// explains — cannot happen.
const DHCP_BUF: usize = 1536;

/// Raises this unit's setup network and stays there.
///
/// Never returns. It reboots once a record is saved, so the normal boot path
/// always starts clean rather than from a half-configured process.
///
/// **The credentials arrive rather than being derived here.** They come from
/// [`crate::provisioning::ap_ssid`] and [`crate::provisioning::ap_password`]
/// exactly as before, but the boot path calls them, because the screen has to
/// show the same two strings this function puts on the air. Deriving them twice
/// could not actually disagree — the functions are pure — but a single
/// derivation is what makes that obvious to a reader.
///
/// The controller is kept alive for the life of setup mode: dropping it takes
/// the network down, and a phone mid-form would simply lose its connection.
pub async fn run(
    spawner: Spawner,
    wifi: WIFI<'static>,
    ssid: &str,
    password: &str,
    mut store: Store,
) -> ! {
    // Printed in full, on purpose. A unit in setup mode has few ways to tell
    // anyone its password, and the console is the one that exists before the
    // network does — the panel says the same thing, for a unit in a kitchen
    // rather than on a bench, and `dev/ap-password.sh <id>` says it off the
    // device entirely, for stickers.
    info!("setup: raising {ssid}");
    info!("setup: password {password}");
    info!("setup: then browse to http://{AP_ADDR}");

    let config = AccessPointConfig::default()
        .with_ssid(ssid)
        // **Not optional.** `AccessPointConfig::default()` is an *open*
        // network, so leaving this out would broadcast a setup portal anyone
        // can join — and the session it protects is the one where the home
        // Wi-Fi password gets typed in. The salted password is worthless
        // without it.
        .with_auth_method(AuthenticationMethod::Wpa2Personal)
        .with_password(password.into());

    // No separate start call: `set_config` calls `esp_wifi_start()` whenever
    // the mode changes, so applying this as the initial config is what brings
    // the network up.
    let (controller, interfaces) = match esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(WifiConfig::AccessPoint(config)),
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

    // Named, not `let _ = ...`. A wildcard pattern drops the controller here
    // and takes the network down with it; a named binding keeps it in this
    // frame, which never returns.
    let controller: WifiController<'static> = controller;

    let stack = start_stack(spawner, interfaces.access_point);

    // What the unit knew before, if anything decoded. It is *not* usable — that
    // is why we are here — but it may still carry this unit's bench-measured
    // detent interval and portion scale, which the form does not ask for and
    // must not silently reset to defaults. See `record_from_form`'s `previous`.
    let previous = previous_record(&mut store);
    if previous.is_some() {
        info!("setup: keeping this unit's measured timings from the old record");
    }

    // Shared because the connection slots below all serve `/save`. Contended
    // only in the moment one of them writes, and that moment ends in a reboot.
    let store = Mutex::<CriticalSectionRawMutex, _>::new(store);

    // `.0` is `!`: every branch diverges, so the tuple is uninhabited and this
    // expression is the function's divergence rather than a value.
    join3(
        serve_dhcp(stack),
        watch_stations(&controller),
        serve_form(stack, &store, previous.as_ref()),
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
/// Without it both look identical — a capture with nothing in it — and the
/// wrong half gets debugged. `wifi::new` already enables these events, so this
/// costs a subscription and no configuration.
///
/// **Read it as evidence, not proof.** `wait_for_access_point_connected_event_async`
/// takes a fresh subscription on every call, so an event published in the gap
/// between one returning and the next subscribing is dropped. A missing
/// association line is therefore *probably* a phone that never joined, and a
/// DHCP line without one above it is not a contradiction. The DHCP lines are
/// the ones that cannot be missed, because they come from the socket itself.
async fn watch_stations(controller: &WifiController<'static>) -> ! {
    loop {
        match controller
            .wait_for_access_point_connected_event_async()
            .await
        {
            Ok(AccessPointStationEventInfo::Connected(info)) => {
                info!("setup: station {} associated", Mac(info.mac));
            }
            Ok(AccessPointStationEventInfo::Disconnected(info)) => {
                info!("setup: station {} left ({:?})", Mac(info.mac), info.reason);
            }
            Err(e) => warn!("setup: station event failed ({e:?})"),
        }
    }
}

/// Brings up the setup network's own embassy-net stack.
///
/// This is a **second** stack. The station-mode one in `main.rs` is not reused
/// and could not be: this is a different interface with a different address,
/// and the two never exist at once anyway, because setup mode is entered
/// before any of the normal tasks are spawned.
///
/// Static, not DHCP, because on this network the unit *is* the server.
fn start_stack(spawner: Spawner, device: Interface<'static>) -> Stack<'static> {
    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;

    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(AP_ADDR, 24),
        // No route off this network, because there is nowhere to go: the unit
        // forwards nothing. What the *clients* are told is a separate
        // question — see `ServerOptions` in `serve_dhcp`.
        gateway: None,
        dns_servers: heapless::Vec::new(),
    });

    // One UDP socket for DHCP plus `CONNECTIONS` TCP ones for the form, and a
    // spare. Too few shows up as `accept failed (InvalidState)` rather than as
    // anything about sockets.
    static RESOURCES: StaticCell<StackResources<{ CONNECTIONS + 2 }>> = StaticCell::new();

    let (stack, runner) =
        embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    spawner.spawn(net_task(runner).expect("failed to create setup net task"));

    stack
}

/// Polls the setup stack.
///
/// A near-twin of `main.rs`'s `net_task`, and separate for a reason that is not
/// style: that one is defined in the binary crate, and a library module cannot
/// spawn a task it cannot name. The duplication is two lines.
#[embassy_executor::task]
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
    // No DNS server is advertised, because there is not one. A name would have
    // nothing to resolve against, and hijacking lookups is the captive portal
    // this design has already declined. The address is typed in by hand.
    let mut gateway = [AP_ADDR];
    let mut options = ServerOptions::new(AP_ADDR, Some(&mut gateway));
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
            // association and then nothing — which `watch_stations` above
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

/// Stops, without spinning.
async fn halt() -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}

// ---------------------------------------------------------------------------
// The form
// ---------------------------------------------------------------------------

/// How much of a request this will hold.
///
/// The head of a phone browser's `POST` runs to a few hundred bytes of headers;
/// the body is six fields, the longest of which is a 64-character password that
/// percent-encoding can treble. 2 KB is comfortable for both, and a request
/// that does not fit is answered rather than silently truncated — a truncated
/// body would parse as a form with fields missing and blame the person typing.
const REQUEST_LEN: usize = 2048;

/// The socket's send buffer. Smaller than a page on purpose: `write_all`
/// refills it as the peer acknowledges, so this only sets how many round trips
/// a response takes, not how large one can be.
const TX_LEN: usize = 2048;

/// How long a connection may sit idle before it is recycled.
///
/// A browser opens connections it then sends nothing on. Each occupies a slot
/// until this expires, so it is short — but not so short that a phone on a weak
/// signal loses a request in flight.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// How many connections are served at once.
///
/// **Not one.** See [`serve_form`]: a single socket makes a browser's own
/// speculative connection block the next real request.
const CONNECTIONS: usize = 3;

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
) -> ! {
    static RX: StaticCell<[[u8; REQUEST_LEN]; CONNECTIONS]> = StaticCell::new();
    static TX: StaticCell<[[u8; TX_LEN]; CONNECTIONS]> = StaticCell::new();
    static REQUEST: StaticCell<[[u8; REQUEST_LEN]; CONNECTIONS]> = StaticCell::new();
    static PAGE: StaticCell<[String<PAGE_LEN>; CONNECTIONS]> = StaticCell::new();

    // Destructured rather than indexed, so each slot below gets its own
    // `&'static mut` and the borrow checker can see they do not alias.
    let [rx0, rx1, rx2] = RX.init([[0; REQUEST_LEN]; CONNECTIONS]);
    let [tx0, tx1, tx2] = TX.init([[0; TX_LEN]; CONNECTIONS]);
    let [rq0, rq1, rq2] = REQUEST.init([[0; REQUEST_LEN]; CONNECTIONS]);
    let [pg0, pg1, pg2] = PAGE.init([String::new(), String::new(), String::new()]);

    info!("setup: form on http://{AP_ADDR}/, {CONNECTIONS} connections");

    join3(
        connection(0, stack, rx0, tx0, rq0, pg0, store, previous),
        connection(1, stack, rx1, tx1, rq1, pg1, store, previous),
        connection(2, stack, rx2, tx2, rq2, pg2, store, previous),
    )
    .await
    .0
}

/// One connection slot: accept, answer, recycle, forever.
///
/// `slot` exists for the log. Two slots working while a third sits idle is the
/// normal picture, and without the number a capture reads as one connection
/// behaving erratically.
#[allow(
    clippy::too_many_arguments,
    reason = "the caller splits one static array per buffer so each slot owns \
    its own; bundling them into a struct would carry the same six fields"
)]
async fn connection(
    slot: u8,
    stack: Stack<'static>,
    rx: &'static mut [u8; REQUEST_LEN],
    tx: &'static mut [u8; TX_LEN],
    request: &'static mut [u8; REQUEST_LEN],
    page: &'static mut String<PAGE_LEN>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
) -> ! {
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(HTTP_TIMEOUT));

    loop {
        if let Err(e) = socket.accept(HTTP_PORT).await {
            warn!("setup: [{slot}] accept failed ({e:?})");
            reset_socket(&mut socket).await;
            continue;
        }

        // `Saved` is the only way out of this loop, and out of setup mode.
        if let Outcome::Saved = handle(slot, &mut socket, request, page, store, previous).await {
            // The browser is told before the unit disappears. Without the
            // flush the reset races the last segment and the phone shows a
            // connection error on what was actually a success.
            socket.close();
            let _ = socket.flush().await;
            Timer::after(Duration::from_millis(250)).await;

            info!("setup: saved, restarting");
            esp_hal::system::software_reset()
        }

        reset_socket(&mut socket).await;
    }
}

/// Returns the socket to a state `accept` will take.
///
/// `close` sends FIN and waits for the peer; `abort` then guarantees the socket
/// is free even if the peer never answers, which a phone that walked out of
/// range will not.
async fn reset_socket(socket: &mut TcpSocket<'_>) {
    socket.close();
    let _ = socket.flush().await;
    socket.abort();
    let _ = socket.flush().await;
}

enum Outcome {
    /// A record is in flash. Reboot into it.
    Saved,
    /// Anything else: the connection is finished with, setup mode continues.
    Continue,
}

/// Reads one request and answers it.
async fn handle(
    slot: u8,
    socket: &mut TcpSocket<'_>,
    request: &mut [u8; REQUEST_LEN],
    page: &mut String<PAGE_LEN>,
    store: &Mutex<CriticalSectionRawMutex, Store>,
    previous: Option<&Record>,
) -> Outcome {
    let Some((head, body)) = read_request(slot, socket, request).await else {
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
            match record_from_form(body, previous) {
                Ok(record) => match store.lock().await.save(&record) {
                    Ok(()) => {
                        info!(
                            "setup: saved {} via {}:{}",
                            record.wifi_ssid, record.mqtt_host, record.mqtt_port
                        );
                        render_saved(page, &record);
                        send(socket, "200 OK", page).await;
                        Outcome::Saved
                    }
                    Err(e) => {
                        // Flash refused the write. Say so on the page rather
                        // than rebooting into a unit that is still unconfigured
                        // and cannot explain why.
                        error!("setup: could not save ({e:?})");
                        render_form(page, body, None, Some("Saving to flash failed."));
                        send(socket, "500 Internal Server Error", page).await;
                        Outcome::Continue
                    }
                },
                Err(e) => {
                    // Not an error on the console: a typo is ordinary, and
                    // warning about every one would make a real fault harder
                    // to spot in a capture.
                    info!("setup: form rejected ({e:?})");
                    render_form(page, body, Some(e), None);
                    // 200, not 400. The body *is* the answer — the form again,
                    // with the message — and a browser shows it either way.
                    send(socket, "200 OK", page).await;
                    Outcome::Continue
                }
            }
        }
        (Method::Other, _) => {
            page.clear();
            let _ = page.push_str("Method not allowed.");
            send(socket, "405 Method Not Allowed", page).await;
            Outcome::Continue
        }
        // `GET /` and everything else. A phone probes several odd paths the
        // moment it joins — `/generate_204`, `/hotspot-detect.html` — looking
        // for internet access, and answering them all with the form means
        // whichever one a browser lands on is the right one. It is not a
        // captive portal: there is no DNS trickery, so the address is still
        // typed in by hand.
        _ => {
            render_form(page, "", None, None);
            send(socket, "200 OK", page).await;
            Outcome::Continue
        }
    }
}

/// Reads until the head parses and the whole body has arrived.
///
/// `None` means the connection produced nothing usable and has been answered
/// if it deserved an answer.
async fn read_request<'b>(
    slot: u8,
    socket: &mut TcpSocket<'_>,
    request: &'b mut [u8; REQUEST_LEN],
) -> Option<(Head, &'b str)> {
    let mut filled = 0;

    let head = loop {
        // Parse before reading: the first read usually carries the whole head,
        // and a `GET` has no body to wait for.
        match parse_head(&request[..filled]) {
            Ok(Some(head)) => break head,
            Ok(None) => {}
            Err(e) => {
                warn!("setup: [{slot}] bad request ({e:?})");
                return None;
            }
        }

        if filled == request.len() {
            warn!("setup: [{slot}] request head too large");
            return None;
        }

        match socket.read(&mut request[filled..]).await {
            // A browser opening a connection and dropping it without a
            // request. Ordinary, and not worth a line in the log.
            Ok(0) => return None,
            Ok(n) => filled += n,
            Err(e) => {
                warn!("setup: [{slot}] read failed ({e:?})");
                return None;
            }
        }
    };

    let want = head.body_at.checked_add(head.content_length)?;
    if want > request.len() {
        warn!(
            "setup: [{slot}] body of {} bytes is too large",
            head.content_length
        );
        return None;
    }

    while filled < want {
        match socket.read(&mut request[filled..]).await {
            Ok(0) => {
                warn!("setup: [{slot}] connection closed mid-body");
                return None;
            }
            Ok(n) => filled += n,
            Err(e) => {
                warn!("setup: [{slot}] read failed ({e:?})");
                return None;
            }
        }
    }

    let body = core::str::from_utf8(&request[head.body_at..want]).ok()?;
    Some((head, body))
}

/// Writes a response, headers and all.
///
/// `Connection: close` on every one. There are only [`CONNECTIONS`] slots, and
/// a browser holding one open with keep-alive would spend a third of them on a
/// connection it has finished with — the same starvation [`serve_form`]
/// describes, just slower to arrive.
async fn send(socket: &mut TcpSocket<'_>, status: &str, body: &str) {
    let mut headers: String<160> = String::new();
    let _ = write!(
        headers,
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );

    if let Err(e) = socket.write_all(headers.as_bytes()).await {
        warn!("setup: write failed ({e:?})");
        return;
    }
    if let Err(e) = socket.write_all(body.as_bytes()).await {
        warn!("setup: write failed ({e:?})");
    }
}
