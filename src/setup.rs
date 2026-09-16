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
//! 3. ⬜ the form on TCP 80

use core::fmt::Write as _;
use core::net::{Ipv4Addr, SocketAddrV4};

use edge_dhcp::server::{Server, ServerOptions};
use edge_dhcp::{Options, Packet};
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
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
    Escaped, FormError, Head, Method, PASSWORD_LEN, Record, ap_password, ap_ssid, field,
    parse_head, record_from_form,
};
use crate::store::Store;

/// The address the unit answers on, and the one printed on the console.
///
/// Fixed rather than negotiated: it is typed in by hand, so it has to be the
/// same on every unit and knowable before the unit is first powered on.
const AP_ADDR: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);

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
/// Never returns. Once slice 3 lands it reboots after saving a record, so the
/// normal boot path always starts clean rather than from a half-configured
/// process.
///
/// The controller is kept alive for the life of setup mode: dropping it takes
/// the network down, and a phone mid-form would simply lose its connection.
pub async fn run(
    spawner: Spawner,
    wifi: WIFI<'static>,
    id: &str,
    secret: &str,
    mut store: Store,
) -> ! {
    let ssid = ap_ssid(id);
    let password = ap_password(secret, id);

    // Printed in full, on purpose. This is a unit that by definition has no
    // other way to tell anyone its password, and a console is the one channel
    // that exists before the network does. `dev/ap-password.sh <id>` prints the
    // same string off the device, for stickers.
    info!("setup: raising {ssid}");
    info!("setup: password {password}");
    info!("setup: then browse to http://{AP_ADDR}");

    let config = AccessPointConfig::default()
        .with_ssid(ssid.as_str())
        // **Not optional.** `AccessPointConfig::default()` is an *open*
        // network, so leaving this out would broadcast a setup portal anyone
        // can join — and the session it protects is the one where the home
        // Wi-Fi password gets typed in. The salted password is worthless
        // without it.
        .with_auth_method(AuthenticationMethod::Wpa2Personal)
        .with_password(password.as_str().into());

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

    // `.0` is `!`: every branch diverges, so the tuple is uninhabited and this
    // expression is the function's divergence rather than a value.
    join3(
        serve_dhcp(stack),
        watch_stations(&controller),
        serve_form(stack, &mut store, previous.as_ref()),
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

    // Three sockets: the DHCP one below, and room for slice 3's listener and
    // the second connection a browser habitually opens alongside it.
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();

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

/// How much rendered HTML this will hold. The page plus six escaped field
/// values, with room to spare.
const PAGE_LEN: usize = 4096;

/// Long enough for a person to be slow, short enough that a connection a
/// browser opened speculatively and abandoned does not hold the only socket.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves the form until a record is saved, then reboots.
///
/// **One connection at a time.** A browser opens several, and the extras are
/// refused while this one is busy — which they survive, because each response
/// says `Connection: close` and the loop is back in `accept` within
/// microseconds. The alternative, a second socket, buys very little and costs a
/// `Store` shared between two writers.
async fn serve_form(stack: Stack<'static>, store: &mut Store, previous: Option<&Record>) -> ! {
    static RX: StaticCell<[u8; REQUEST_LEN]> = StaticCell::new();
    static TX: StaticCell<[u8; PAGE_LEN]> = StaticCell::new();
    static REQUEST: StaticCell<[u8; REQUEST_LEN]> = StaticCell::new();
    static PAGE: StaticCell<String<PAGE_LEN>> = StaticCell::new();

    let mut socket = TcpSocket::new(stack, RX.init([0; REQUEST_LEN]), TX.init([0; PAGE_LEN]));
    socket.set_timeout(Some(HTTP_TIMEOUT));

    let request = REQUEST.init([0; REQUEST_LEN]);
    let page = PAGE.init(String::new());

    info!("setup: form on http://{AP_ADDR}/");

    loop {
        if let Err(e) = socket.accept(HTTP_PORT).await {
            warn!("setup: accept failed ({e:?})");
            reset_socket(&mut socket).await;
            continue;
        }

        // `Saved` is the only way out of this loop, and out of setup mode.
        if let Outcome::Saved = handle(&mut socket, request, page, store, previous).await {
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
    socket: &mut TcpSocket<'_>,
    request: &mut [u8; REQUEST_LEN],
    page: &mut String<PAGE_LEN>,
    store: &mut Store,
    previous: Option<&Record>,
) -> Outcome {
    let Some((head, body)) = read_request(socket, request).await else {
        return Outcome::Continue;
    };

    match (head.method, head.path.as_str()) {
        (Method::Post, "/save") => {
            match record_from_form(body, previous) {
                Ok(record) => match store.save(&record) {
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
                warn!("setup: bad request ({e:?})");
                return None;
            }
        }

        if filled == request.len() {
            warn!("setup: request head too large");
            return None;
        }

        match socket.read(&mut request[filled..]).await {
            // A browser opening a connection and dropping it without a
            // request. Ordinary, and not worth a line in the log.
            Ok(0) => return None,
            Ok(n) => filled += n,
            Err(e) => {
                warn!("setup: read failed ({e:?})");
                return None;
            }
        }
    };

    let want = head.body_at.checked_add(head.content_length)?;
    if want > request.len() {
        warn!("setup: body of {} bytes is too large", head.content_length);
        return None;
    }

    while filled < want {
        match socket.read(&mut request[filled..]).await {
            Ok(0) => {
                warn!("setup: connection closed mid-body");
                return None;
            }
            Ok(n) => filled += n,
            Err(e) => {
                warn!("setup: read failed ({e:?})");
                return None;
            }
        }
    }

    let body = core::str::from_utf8(&request[head.body_at..want]).ok()?;
    Some((head, body))
}

/// Writes a response, headers and all.
///
/// `Connection: close` on every one, because this serves a single socket and a
/// browser holding it open with keep-alive would lock out its own next request.
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

/// The page, with whatever was last typed still in the boxes.
///
/// `submitted` is the body of the rejected `POST`, or `""` for a first visit.
/// Re-reading the fields out of it rather than carrying a parsed struct means
/// a body that failed to parse *as a whole* can still give back the fields that
/// were fine — which is exactly the case this runs in.
///
/// **Passwords are filled back in too.** They are the longest things on the
/// form and the most miserable to retype on a phone, and clearing them on a
/// typo in the port field is how someone ends up giving up. The response goes
/// over the unit's own WPA2 link to the person who just typed it, in answer to
/// a request that carried the same password, so echoing it back reaches nobody
/// new. `Cache-Control: no-store` in [`send`] keeps it out of the browser's
/// history.
fn render_form(
    page: &mut String<PAGE_LEN>,
    submitted: &str,
    error: Option<FormError>,
    failure: Option<&str>,
) {
    page.clear();

    let _ = page.push_str(
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>cat-feeder setup</title><style>\
         body{font:16px/1.5 system-ui,sans-serif;margin:0;padding:1.5rem;\
         max-width:26rem;background:#faf9f7;color:#222}\
         h1{font-size:1.25rem;margin:0 0 1rem}\
         label{display:block;margin:.75rem 0 .2rem;font-weight:600;font-size:.9rem}\
         input{width:100%;box-sizing:border-box;padding:.55rem;font-size:1rem;\
         border:1px solid #bbb;border-radius:.3rem;background:#fff}\
         button{margin-top:1.25rem;width:100%;padding:.7rem;font-size:1rem;\
         border:0;border-radius:.3rem;background:#2f6f4f;color:#fff}\
         .err{background:#fdecea;border:1px solid #d9534f;border-radius:.3rem;\
         padding:.6rem;margin-bottom:1rem}\
         .hint{color:#666;font-size:.8rem;margin:.2rem 0 0}\
         </style></head><body><h1>cat-feeder setup</h1>",
    );

    // The failure, if there was one, above the fields rather than beside them:
    // there is only ever one, and a phone screen is short.
    if let Some(message) = failure {
        let _ = write!(page, "<p class=err>{}</p>", Escaped(message));
    } else if let Some(error) = error {
        let _ = write!(page, "<p class=err>{error}</p>");
    }

    let _ = page.push_str("<form method=post action=/save>");

    text_field(page, "wifi_ssid", "Wi-Fi network", submitted, "text", None);
    text_field(
        page,
        "wifi_password",
        "Wi-Fi password",
        submitted,
        "password",
        Some("Leave empty for an open network."),
    );
    text_field(
        page,
        "mqtt_host",
        "Broker address",
        submitted,
        "text",
        Some("An IP address such as 192.168.1.10. Names will not work."),
    );
    text_field(page, "mqtt_port", "Broker port", submitted, "text", None);
    text_field(
        page,
        "mqtt_user",
        "Broker username",
        submitted,
        "text",
        None,
    );
    text_field(
        page,
        "mqtt_password",
        "Broker password",
        submitted,
        "password",
        None,
    );

    let _ = page.push_str("<button type=submit>Save and restart</button></form></body></html>");
}

/// One labelled input, with its previous value escaped back into it.
///
/// The `name` is the one [`record_from_form`] looks for. They are written out
/// here rather than derived, so this file and `provisioning.rs` can be grepped
/// for the same six strings.
fn text_field(
    page: &mut String<PAGE_LEN>,
    name: &str,
    label: &str,
    submitted: &str,
    kind: &str,
    hint: Option<&str>,
) {
    // A field that failed to decode comes back empty rather than taking the
    // whole page down with it: the rest of the form is still worth showing.
    let value = field::<PASSWORD_LEN>(submitted, name)
        .ok()
        .flatten()
        .unwrap_or_default();

    let _ = write!(
        page,
        "<label for={name}>{label}</label>\
         <input id={name} name={name} type={kind} value=\"{}\"",
        Escaped(&value)
    );
    // The port is the only numeric one, and a phone showing a number pad for
    // it saves a keyboard switch.
    if name == "mqtt_port" {
        let _ = page.push_str(" inputmode=numeric");
    }
    let _ = page.push_str(">");

    if let Some(hint) = hint {
        let _ = write!(page, "<p class=hint>{}</p>", Escaped(hint));
    }
}

/// The page shown once, on the way out.
fn render_saved(page: &mut String<PAGE_LEN>, record: &Record) {
    page.clear();
    let _ = write!(
        page,
        "<!doctype html><html lang=en><head><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>cat-feeder setup</title><style>\
         body{{font:16px/1.5 system-ui,sans-serif;margin:0;padding:1.5rem;\
         max-width:26rem;background:#faf9f7;color:#222}}\
         </style></head><body><h1>Saved</h1>\
         <p>This feeder is restarting and will join <b>{}</b>, then connect to \
         the broker at <b>{}:{}</b>.</p>\
         <p>Its setup network is about to disappear — that is what success \
         looks like. Rejoin your own Wi-Fi.</p>\
         <p>If it does not appear in Home Assistant, hold the button through a \
         power cycle to erase and start again.</p>\
         </body></html>",
        Escaped(&record.wifi_ssid),
        Escaped(&record.mqtt_host),
        record.mqtt_port,
    );
}
