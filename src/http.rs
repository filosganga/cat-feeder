//! The sockets under both web servers: setup mode's form and a configured
//! unit's admin page.
//!
//! Reading a request, writing a response, recycling a socket, and the buffers
//! each connection slot owns. What a request *means* is decided elsewhere —
//! [`crate::provisioning`] and [`crate::admin`], both pure — so this is only
//! bytes in and out.
//!
//! **Only one of the two servers ever runs.** Setup mode never returns and a
//! configured unit never enters it, so they share one set of buffers rather
//! than each reserving its own: [`slots`] hands them out once per boot.

use core::fmt::Write as _;

use embassy_net::tcp::TcpSocket;
use embassy_time::Duration;
use embedded_io_async::Write as _;
use heapless::String;
use log::warn;
use static_cell::StaticCell;

use crate::provisioning::{Head, PAGE_LEN, parse_head};

/// Plain HTTP, on both. There is no certificate a unit could present that a
/// browser would not shout about.
pub const PORT: u16 = 80;

/// How much of a request this will hold.
///
/// The head of a phone browser's `POST` runs to a few hundred bytes of headers;
/// the setup form's body is six fields, the longest of which is a 64-character
/// password that percent-encoding can treble. 2 KB is comfortable for both, and
/// a request that does not fit is answered rather than silently truncated — a
/// truncated body would parse as a form with fields missing and blame the
/// person typing.
pub const REQUEST_LEN: usize = 2048;

/// The socket's send buffer. Smaller than a page on purpose: `write_all`
/// refills it as the peer acknowledges, so this only sets how many round trips
/// a response takes, not how large one can be.
pub const TX_LEN: usize = 2048;

/// How long a connection may sit idle before it is recycled.
///
/// A browser opens connections it then sends nothing on. Each occupies a slot
/// until this expires, so it is short — but not so short that a phone on a weak
/// signal loses a request in flight.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// How many connections are served at once.
///
/// **Not one.** A browser opens more than it uses, and sends nothing on some of
/// them. With a single socket the next real request is refused with a RST while
/// an idle one is still being waited on — which is what "this site can't be
/// reached" looked like when Save was pressed, moments after the GET that drew
/// the form had worked perfectly.
pub const CONNECTIONS: usize = 3;

/// Everything one connection slot owns.
pub struct Slot {
    pub rx: &'static mut [u8; REQUEST_LEN],
    pub tx: &'static mut [u8; TX_LEN],
    pub request: &'static mut [u8; REQUEST_LEN],
    pub page: &'static mut String<PAGE_LEN>,
}

/// The [`CONNECTIONS`] slots' buffers. Callable **once** per boot: a second
/// call panics, which is the point — two servers sharing them would be two
/// servers writing into each other's pages.
pub fn slots() -> [Slot; CONNECTIONS] {
    static RX: StaticCell<[[u8; REQUEST_LEN]; CONNECTIONS]> = StaticCell::new();
    static TX: StaticCell<[[u8; TX_LEN]; CONNECTIONS]> = StaticCell::new();
    static REQUEST: StaticCell<[[u8; REQUEST_LEN]; CONNECTIONS]> = StaticCell::new();
    static PAGE: StaticCell<[String<PAGE_LEN>; CONNECTIONS]> = StaticCell::new();

    // Destructured rather than indexed, so each slot gets its own
    // `&'static mut` and the borrow checker can see they do not alias.
    let [rx0, rx1, rx2] = RX.init([[0; REQUEST_LEN]; CONNECTIONS]);
    let [tx0, tx1, tx2] = TX.init([[0; TX_LEN]; CONNECTIONS]);
    let [rq0, rq1, rq2] = REQUEST.init([[0; REQUEST_LEN]; CONNECTIONS]);
    let [pg0, pg1, pg2] = PAGE.init([String::new(), String::new(), String::new()]);

    [
        Slot {
            rx: rx0,
            tx: tx0,
            request: rq0,
            page: pg0,
        },
        Slot {
            rx: rx1,
            tx: tx1,
            request: rq1,
            page: pg1,
        },
        Slot {
            rx: rx2,
            tx: tx2,
            request: rq2,
            page: pg2,
        },
    ]
}

/// Returns the socket to a state `accept` will take.
///
/// `close` sends FIN and waits for the peer; `abort` then guarantees the socket
/// is free even if the peer never answers, which a phone that walked out of
/// range will not.
pub async fn reset(socket: &mut TcpSocket<'_>) {
    socket.close();
    let _ = socket.flush().await;
    socket.abort();
    let _ = socket.flush().await;
}

/// Reads until the head parses and the whole body has arrived.
///
/// `None` means the connection produced nothing usable. `tag` and `slot` are
/// for the log: two slots working while a third sits idle is the normal
/// picture, and without the number a capture reads as one connection behaving
/// erratically.
pub async fn read_request<'b>(
    tag: &str,
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
                warn!("{tag}: [{slot}] bad request ({e:?})");
                return None;
            }
        }

        if filled == request.len() {
            warn!("{tag}: [{slot}] request head too large");
            return None;
        }

        match socket.read(&mut request[filled..]).await {
            // A browser opening a connection and dropping it without a
            // request. Ordinary, and not worth a line in the log.
            Ok(0) => return None,
            Ok(n) => filled += n,
            Err(e) => {
                warn!("{tag}: [{slot}] read failed ({e:?})");
                return None;
            }
        }
    };

    let want = head.body_at.checked_add(head.content_length)?;
    if want > request.len() {
        warn!(
            "{tag}: [{slot}] body of {} bytes is too large",
            head.content_length
        );
        return None;
    }

    while filled < want {
        match socket.read(&mut request[filled..]).await {
            Ok(0) => {
                warn!("{tag}: [{slot}] connection closed mid-body");
                return None;
            }
            Ok(n) => filled += n,
            Err(e) => {
                warn!("{tag}: [{slot}] read failed ({e:?})");
                return None;
            }
        }
    }

    let body = core::str::from_utf8(&request[head.body_at..want]).ok()?;
    Some((head, body))
}

/// Writes a response, headers and all. `extra` is more header lines, each
/// ending `\r\n` — a `WWW-Authenticate` challenge, say — or `""`.
///
/// `Connection: close` on every one. There are only [`CONNECTIONS`] slots, and
/// a browser holding one open with keep-alive would spend a third of them on a
/// connection it has finished with — the same starvation described there, just
/// slower to arrive.
///
/// `Cache-Control: no-store` on every one too: the setup form can echo a
/// password back, and the admin page shows a unit's network, neither of which
/// belongs in a browser's history.
pub async fn send(tag: &str, socket: &mut TcpSocket<'_>, status: &str, extra: &str, body: &str) {
    let mut headers: String<256> = String::new();
    let _ = write!(
        headers,
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         {extra}\
         Connection: close\r\n\r\n",
        body.len()
    );

    if let Err(e) = socket.write_all(headers.as_bytes()).await {
        warn!("{tag}: write failed ({e:?})");
        return;
    }
    if let Err(e) = socket.write_all(body.as_bytes()).await {
        warn!("{tag}: write failed ({e:?})");
    }
}
