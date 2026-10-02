//! The admin page, served on the house network by a configured unit — v2's
//! point 5.
//!
//! What the page says and accepts, and who may use it, is decided in
//! [`crate::admin`], which is pure and host-tested; this is the sockets and the
//! wiring to the rest of the unit. The sockets themselves are
//! [`crate::http`]'s, shared with setup mode's form.
//!
//! | Request | Answer |
//! |---|---|
//! | anything, without the password | `401` and a Basic challenge |
//! | `GET /` | the page: status, meals, network |
//! | `POST /feed` | portions onto the feed queue, like the knob's `Feed` |
//! | `POST /pause` | pause or resume the schedule, into flash, like the knob's `Pause` |
//! | `POST /schedule` | the meals, through the schedule task — no restart |
//! | `POST /clock` | a hand-set time, like the knob's `Clock` |
//! | `POST /calibration` | the detent and portion scale, into flash and in force |
//! | `POST /calibrate` | start a calibration run; the page follows it |
//! | `POST /network` | Wi-Fi and broker, into flash, then a restart |
//! | `POST /update` | a firmware image into the idle slot, then a restart (ADR-0022) |
//! | a `POST` from another site | `403` |
//! | any other path | `404` |
//!
//! **One more writer of the same record, not a second way in.** The boot
//! decision is still *no valid record → setup mode*; this page only ever
//! rewrites a record that works, and a wrong one is recovered the way any
//! other is, with the button held through power-on.

use embassy_futures::join::join3;
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer};
use heapless::String;
use log::{info, warn};

use crate::admin::{
    CHALLENGE, Network, Notice, ScheduleFormError, Status, authorized, calibration_from_form,
    clock_form, feed_from_form, network_from_form, pause_from_form, render_page, render_restarting,
    same_origin, schedule_from_form,
};
use crate::config::AP_SECRET;
use crate::events::{Event, Source};
use crate::firmware::{self, SECTOR, Target};
use crate::http;
use crate::provisioning::{Head, Method, PAGE_LEN};
use crate::schedule::ScheduleCommand;
use crate::store::SharedStore;
use crate::tz::Zone;
use crate::update::{ImageCheck, ImageError, MARK_PREFIX, Sectors, secret_mark};
use crate::wiring::Bus;

use core::sync::atomic::Ordering;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

/// What the page needs that is fixed for the life of the process.
#[derive(Clone, Copy)]
pub struct Unit {
    pub id: &'static str,
    pub version: &'static str,
    /// The derived password — the setup network's, the sticker's.
    pub password: &'static str,
    /// As configured at boot. A change saved here restarts the unit, so the
    /// running configuration never differs from this while the page is up.
    pub network: Network<'static>,
}

/// How long `POST /schedule` waits for the schedule task to take the meals
/// before answering. It drains its queue once a second, so this is generous;
/// past it the page says so rather than showing the old meals as saved.
const APPLY_WAIT: Duration = Duration::from_secs(3);

/// This build's `ap_secret`, marked, so an upload can be checked for it and so
/// this image itself carries it for the next one (`update.rs`). A `static`
/// referenced at run time, so the bytes are in the image exactly once.
static SECRET_MARK: [u8; MARK_PREFIX.len() + AP_SECRET.len()] = secret_mark(AP_SECRET);

/// The sector an upload is assembled in. Behind a lock that is only ever
/// tried, never waited on: holding it is what makes an upload the only one.
static SECTORS: Mutex<CriticalSectionRawMutex, Sectors<SECTOR>> = Mutex::new(Sectors::new());

/// Serves the admin page forever.
pub async fn run(
    stack: Stack<'static>,
    store: &'static SharedStore,
    unit: Unit,
    bus: &'static Bus,
) -> ! {
    let [s0, s1, s2] = http::slots();
    info!(
        "web: admin page on port {}, {} connections",
        http::PORT,
        http::CONNECTIONS
    );

    join3(
        connection(0, stack, s0, store, unit, bus),
        connection(1, stack, s1, store, unit, bus),
        connection(2, stack, s2, store, unit, bus),
    )
    .await
    .0
}

/// One connection slot: accept, answer, recycle, forever.
async fn connection(
    slot: u8,
    stack: Stack<'static>,
    buffers: http::Slot,
    store: &'static SharedStore,
    unit: Unit,
    bus: &'static Bus,
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
            warn!("web: [{slot}] accept failed ({e:?})");
            http::reset(&mut socket).await;
            continue;
        }

        if let Some((head, filled)) = http::read_head("web", slot, &mut socket, request).await {
            // An image is far larger than a request buffer, so it is streamed
            // from the socket rather than read whole.
            let restart = if head.method == Method::Post && head.path == "/update" {
                upload(
                    slot,
                    &mut socket,
                    &head,
                    request,
                    filled,
                    page,
                    store,
                    unit,
                    bus,
                )
                .await
            } else if let Some(body) =
                http::read_body("web", slot, &mut socket, request, &head, filled).await
            {
                handle(slot, &mut socket, &head, body, page, store, unit, bus).await
            } else {
                false
            };
            if restart {
                // The browser is told before the unit disappears. Without the
                // flush the reset races the last segment and the browser shows
                // a connection error on what was actually a success.
                socket.close();
                let _ = socket.flush().await;
                Timer::after(Duration::from_millis(250)).await;
                info!("web: restarting");
                esp_hal::system::software_reset()
            }
        }

        http::reset(&mut socket).await;
    }
}

/// The password, then the `Origin` of a `POST` (ADR-0017). `false` means the
/// refusal has been sent.
async fn admit(
    slot: u8,
    socket: &mut TcpSocket<'_>,
    head: &Head,
    page: &mut String<PAGE_LEN>,
    unit: Unit,
) -> bool {
    if !authorized(&head.authorization, unit.password) {
        // Not logged per request: a browser's first request always lands here,
        // before it has asked anyone for the password.
        page.clear();
        let _ = page.push_str("This feeder's password is on its sticker and its setup screen.");
        http::send("web", socket, "401 Unauthorized", CHALLENGE, page).await;
        return false;
    }

    info!("web: [{slot}] {:?} {}", head.method, head.path);

    if head.method == Method::Post && !same_origin(&head.origin, &head.host) {
        warn!("web: [{slot}] refused a POST from {}", head.origin);
        page.clear();
        let _ = page.push_str("Refused: this form was sent from another site.");
        http::send("web", socket, "403 Forbidden", "", page).await;
        return false;
    }
    true
}

/// Answers one request. `true` means a new record is in flash: restart.
#[allow(clippy::too_many_arguments, reason = "one call site, all of it wiring")]
async fn handle(
    slot: u8,
    socket: &mut TcpSocket<'_>,
    head: &Head,
    body: &str,
    page: &mut String<PAGE_LEN>,
    store: &'static SharedStore,
    unit: Unit,
    bus: &'static Bus,
) -> bool {
    if !admit(slot, socket, head, page, unit).await {
        return false;
    }

    match (head.method, head.path.as_str()) {
        (Method::Get, "/") => {
            show(page, unit, bus, "", None);
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/schedule") => {
            let outcome = save_schedule(body, bus).await;
            show(page, unit, bus, "", Some(outcome));
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/feed") => {
            let mut text: String<48> = String::new();
            let notice = match feed_from_form(body) {
                Ok(portions) => match bus.feed.try_send(portions) {
                    Ok(()) => {
                        info!("web: feed {portions}");
                        bus.report(Event::Manual {
                            source: Source::Web,
                            portions,
                        });
                        let _ = core::fmt::Write::write_fmt(
                            &mut text,
                            format_args!("Feeding {portions}."),
                        );
                        Notice::Done(text.as_str())
                    }
                    Err(_) => {
                        warn!("web: feed queue full, {portions} portions dropped");
                        Notice::Problem("The feed queue is full. Try again in a moment.")
                    }
                },
                Err(message) => Notice::Problem(message),
            };
            show(page, unit, bus, "", Some(notice));
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/pause") => {
            let notice = match pause_from_form(body) {
                Ok(paused) => save_pause(paused, store, bus).await,
                Err(message) => Notice::Problem(message),
            };
            show(page, unit, bus, "", Some(notice));
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/clock") => {
            let notice = match clock_form(body) {
                Ok(form) => {
                    let zone_saved = match form.zone {
                        Some(zone) => save_zone(zone, store, bus).await,
                        None => Ok(false),
                    };
                    if let Some(wall) = form.at {
                        bus.set_clock_by_hand(wall);
                        info!("web: clock set by hand to {wall}");
                    }
                    match (zone_saved, form.at.is_some()) {
                        (Err(message), _) => Notice::Problem(message),
                        (Ok(true), true) => Notice::Done("Clock and timezone set."),
                        (Ok(true), false) => Notice::Done("Timezone set."),
                        (Ok(false), true) => Notice::Done("Clock set."),
                        (Ok(false), false) => Notice::Done("Nothing changed."),
                    }
                }
                Err(message) => Notice::Problem(message),
            };
            // The schedule task takes the time on its next tick; drawn before
            // then, the page would still say the clock is not set.
            Timer::after(Duration::from_millis(1_100)).await;
            show(page, unit, bus, "", Some(notice));
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/calibration") => {
            let notice = save_calibration(body, store, bus).await;
            show(page, unit, bus, "", Some(notice));
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/calibrate") => {
            // The feeder runs it at its next idle moment, never over a meal.
            // The page redraws itself every two seconds while it turns.
            bus.calibrate.signal(());
            info!("web: calibration started");
            Timer::after(Duration::from_millis(300)).await;
            show(
                page,
                unit,
                bus,
                "",
                Some(Notice::Done("Calibration started.")),
            );
            http::send("web", socket, "200 OK", "", page).await;
            false
        }
        (Method::Post, "/network") => {
            let saved = save_network(body, page, store, unit, bus).await;
            http::send("web", socket, "200 OK", "", page).await;
            saved
        }
        (Method::Get, _) => {
            page.clear();
            let _ = page.push_str("Not here.");
            http::send("web", socket, "404 Not Found", "", page).await;
            false
        }
        _ => {
            page.clear();
            let _ = page.push_str("Method not allowed.");
            http::send("web", socket, "405 Method Not Allowed", "", page).await;
            false
        }
    }
}

/// Renders the page from what the unit holds now.
#[inline(never)]
fn show(
    page: &mut String<PAGE_LEN>,
    unit: Unit,
    bus: &Bus,
    submitted: &str,
    notice: Option<Notice>,
) {
    let zone = bus.zone.get();
    let status = Status {
        id: unit.id,
        version: unit.version,
        now: bus.now.get(),
        paused: bus.is_paused(),
        jammed: bus.status.jammed(),
        next: bus.next.get(),
        last_fed: bus.last_fed.get(),
        calibration: bus.calibration.get(),
        progress: bus.calibration_progress.get(),
        zone: zone.as_ref(),
    };
    let held = bus.held.get();
    render_page(
        page,
        &status,
        held.as_ref(),
        &unit.network,
        submitted,
        notice,
    );
}

/// The notice after a schedule POST. `'static` because every message is.
async fn save_schedule(body: &str, bus: &'static Bus) -> Notice<'static> {
    let schedule = match schedule_from_form(body) {
        Ok(schedule) => schedule,
        Err(e) => {
            info!("web: meals rejected ({e:?})");
            return Notice::Problem(schedule_message(e));
        }
    };

    // Through the same queue as a command from Home Assistant, so the schedule
    // task stays the only writer: flash first, then in force, then echoed on
    // `feeder/<id>/schedule/state` — which moves the `Meal n` entities too.
    if bus
        .schedule
        .try_send(ScheduleCommand::Replace(schedule.clone()))
        .is_err()
    {
        warn!("web: schedule queue full, meals not saved");
        return Notice::Problem("The feeder is busy. Try again in a moment.");
    }

    // Wait for it to be taken, so the page drawn next shows the new meals
    // rather than the old ones with a message claiming otherwise.
    let deadline = Instant::now() + APPLY_WAIT;
    while Instant::now() < deadline {
        if bus.held.get().as_ref() == Some(&schedule) {
            return Notice::Done("Meals saved.");
        }
        Timer::after(Duration::from_millis(100)).await;
    }
    Notice::Problem("Sent, but not yet in force. Reload in a moment to check.")
}

/// The form error as the page says it. Fixed strings, so the notice can
/// borrow them for as long as it likes; the row number goes in the log.
fn schedule_message(e: ScheduleFormError) -> &'static str {
    match e {
        ScheduleFormError::MissingTime(_) => {
            "A meal has no time but one below it does. Meals are numbered by \
             position — set a meal's portions to 0 to stop it instead."
        }
        ScheduleFormError::BadTime(_) => "A meal's time is not a time of day.",
        ScheduleFormError::BadPortions(_) => "A meal's portions are out of range.",
    }
}

/// Pauses or resumes the schedule through [`Bus::set_pause`], the knob's path:
/// flash first, then in force, then Home Assistant's switch follows from the
/// state payload. Works with no broker at all — the unit owns its pause.
async fn save_pause(
    paused: bool,
    store: &'static SharedStore,
    bus: &'static Bus,
) -> Notice<'static> {
    let word = if paused { "paused" } else { "resumed" };
    match bus.set_pause(&mut *store.lock().await, paused) {
        Ok(true) => {
            info!("web: schedule {word}");
            Notice::Done(if paused {
                "Schedule paused."
            } else {
                "Schedule resumed."
            })
        }
        Ok(false) => Notice::Done("Nothing changed."),
        Err(e) => {
            warn!("web: pause not stored ({e:?})");
            Notice::Problem("Saving to flash failed; nothing changed.")
        }
    }
}

/// Stores a timezone and puts it in force. `Ok(true)` if it changed.
///
/// Flash first, then `Bus::zone`, which the schedule task reads every tick:
/// with no live time from Home Assistant, the clock moves into the zone's
/// offset within a second, and the RTC follows.
async fn save_zone(
    zone: Zone,
    store: &'static SharedStore,
    bus: &'static Bus,
) -> Result<bool, &'static str> {
    if bus.zone.get().as_ref() == Some(&zone) {
        return Ok(false);
    }
    match store.lock().await.save_zone(Some(&zone)) {
        Ok(()) => {
            info!("web: timezone {} ({})", zone.name, zone.rule);
            bus.zone.set(Some(zone));
            Ok(true)
        }
        Err(e) => {
            warn!("web: timezone not saved ({e:?})");
            Err("Saving the timezone to flash failed; nothing changed.")
        }
    }
}

/// Stores a calibration and puts it in force, the way the knob's `Detent` and
/// `Portion` do: flash first, then `Bus::calibration`, which the feeder task
/// applies at its next idle moment and the knob's menu reads before its next
/// input.
async fn save_calibration(
    body: &str,
    store: &'static SharedStore,
    bus: &'static Bus,
) -> Notice<'static> {
    let current = bus.calibration.get();
    let wanted = match calibration_from_form(body, current) {
        Ok(wanted) => wanted,
        Err(message) => return Notice::Problem(message),
    };
    if wanted == current {
        return Notice::Done("Nothing changed.");
    }
    let saved = store.lock().await.update(|record| {
        record.detent_ms = wanted.detent_ms;
        record.portion_scale_pct = wanted.portion_scale_pct;
    });
    match saved {
        Ok(()) => {
            bus.calibration.set(wanted);
            info!(
                "web: saved detent {}ms, portion scale {}%",
                wanted.detent_ms, wanted.portion_scale_pct
            );
            Notice::Done("Calibration saved; in force from the next feed.")
        }
        Err(e) => {
            warn!("web: calibration not saved ({e:?})");
            Notice::Problem("Saving to flash failed; nothing changed.")
        }
    }
}

/// Writes the network form into the record. `true` when it was saved and the
/// page drawn is the restarting one.
async fn save_network(
    body: &str,
    page: &mut String<PAGE_LEN>,
    store: &'static SharedStore,
    unit: Unit,
    bus: &'static Bus,
) -> bool {
    let mut store = store.lock().await;
    match network_step(&mut store, body, page) {
        Step::Saved => true,
        Step::Unchanged => {
            show(page, unit, bus, "", Some(Notice::Done("Nothing changed.")));
            false
        }
        Step::Rejected(message) => {
            show(
                page,
                unit,
                bus,
                body,
                Some(Notice::Problem(message.as_str())),
            );
            false
        }
    }
}

enum Step {
    Saved,
    Unchanged,
    Rejected(String<160>),
}

/// The flash half of [`save_network`], out of line: a `Record` is a few
/// hundred bytes and two of them are live here, which must not land in the
/// async frame every connection slot keeps.
#[inline(never)]
fn network_step(store: &mut crate::store::Store, body: &str, page: &mut String<PAGE_LEN>) -> Step {
    use core::fmt::Write as _;

    let current = match store.load() {
        Ok(record) => record,
        Err(e) => {
            warn!("web: could not read the record ({e:?})");
            return Step::Rejected(
                String::try_from("Could not read the stored configuration.").unwrap_or_default(),
            );
        }
    };
    let record = match network_from_form(body, &current) {
        Ok(record) => record,
        Err(e) => {
            info!("web: network form rejected ({e:?})");
            let mut message = String::new();
            let _ = write!(message, "{e}");
            return Step::Rejected(message);
        }
    };
    if record == current {
        return Step::Unchanged;
    }
    match store.save(&record) {
        Ok(()) => {
            info!(
                "web: saved {} via {}:{}",
                record.wifi_ssid, record.mqtt_host, record.mqtt_port
            );
            render_restarting(page, &record);
            Step::Saved
        }
        Err(e) => {
            warn!("web: could not save ({e:?})");
            Step::Rejected(
                String::try_from("Saving to flash failed; nothing changed.").unwrap_or_default(),
            )
        }
    }
}

/// Answers a plain-text result to an upload: `curl` is the client, not a
/// browser, so there is no page to draw.
async fn reply(socket: &mut TcpSocket<'_>, page: &mut String<PAGE_LEN>, status: &str, text: &str) {
    page.clear();
    let _ = page.push_str(text);
    http::send("web", socket, status, "", page).await;
}

/// `POST /update`: a firmware image, written into the idle slot as it
/// arrives, checked, then selected for the next boot (ADR-0022). `true` means
/// it is selected: restart into it.
///
/// Nothing is selected unless the whole image arrived and passed
/// [`ImageCheck`]; a failed upload leaves a half-written idle slot, which
/// nothing boots.
#[allow(clippy::too_many_arguments, reason = "one call site, all of it wiring")]
async fn upload(
    slot: u8,
    socket: &mut TcpSocket<'_>,
    head: &Head,
    request: &mut [u8; http::REQUEST_LEN],
    filled: usize,
    page: &mut String<PAGE_LEN>,
    store: &'static SharedStore,
    unit: Unit,
    bus: &'static Bus,
) -> bool {
    if !admit(slot, socket, head, page, unit).await {
        return false;
    }

    let Ok(mut sectors) = SECTORS.try_lock() else {
        reply(
            socket,
            page,
            "409 Conflict",
            "Another update is in progress.",
        )
        .await;
        return false;
    };
    sectors.reset();

    let target = match firmware::idle_slot(&mut *store.lock().await) {
        Ok(target) => target,
        Err(e) => {
            warn!("web: [{slot}] update refused: {e:?}");
            reply(socket, page, "500 Internal Server Error", e.message()).await;
            return false;
        }
    };
    let mut check = match ImageCheck::new(head.content_length, target.len as usize, &SECRET_MARK) {
        Ok(check) => check,
        Err(e) => {
            warn!("web: [{slot}] update refused: {e:?}");
            reply(socket, page, "400 Bad Request", e.message()).await;
            return false;
        }
    };

    // Checked and claimed with no await between, so a turn cannot start in
    // the gap: the feeder starts one only from its own loop, and checks the
    // claim synchronously just before it does.
    if bus.status.feeding() {
        reply(
            socket,
            page,
            "409 Conflict",
            "Feeding now; try again when it stops.",
        )
        .await;
        return false;
    }
    let busy = FlashBusy::claim(bus);

    info!(
        "web: [{slot}] update: {} bytes into {:?} at {:#x}",
        head.content_length, target.slot, target.offset
    );
    let started = Instant::now();
    let written = receive(
        socket,
        head,
        request,
        filled,
        &mut check,
        &mut sectors,
        store,
        target,
    )
    .await;

    let outcome = match written {
        Ok(()) => check.finish().map_err(Refusal::Image),
        Err(e) => Err(e),
    };
    if let Err(e) = outcome {
        drop(busy);
        warn!("web: [{slot}] update failed: {e:?}");
        let (status, text) = match e {
            Refusal::Image(e) => ("400 Bad Request", e.message()),
            Refusal::Flash(e) => ("500 Internal Server Error", e.message()),
        };
        reply(socket, page, status, text).await;
        return false;
    }

    // `otadata` is flash too, so the claim lasts until it is written.
    let selected = firmware::select(&mut *store.lock().await, target);
    drop(busy);
    if let Err(e) = selected {
        warn!("web: [{slot}] update written but not selected: {e:?}");
        reply(socket, page, "500 Internal Server Error", e.message()).await;
        return false;
    }
    info!(
        "web: [{slot}] update: {:?} selected after {} ms",
        target.slot,
        started.elapsed().as_millis()
    );

    reply(
        socket,
        page,
        "200 OK",
        "Updated. Restarting into the new firmware; if it cannot reach the broker \
         it goes back to this one by itself.",
    )
    .await;

    // Feeds held during the write run now, on this firmware, before the
    // restart drops anything still queued. No deadline: a turn always ends,
    // at its last click or at the jam timeout, and cutting one short would
    // lose a meal already marked consumed.
    while !idle(bus) {
        Timer::after(Duration::from_millis(200)).await;
    }
    true
}

/// Nothing turning, queued or held: a restart now loses no feed.
pub fn idle(bus: &Bus) -> bool {
    !bus.status.feeding() && bus.feed.is_empty() && !bus.feed_held.load(Ordering::Relaxed)
}

/// [`Bus::flash_busy`] for as long as this lives, on every way out of an
/// upload, including the connection dropping mid-write.
struct FlashBusy(&'static Bus);

impl FlashBusy {
    fn claim(bus: &'static Bus) -> Self {
        bus.flash_busy.store(true, Ordering::Relaxed);
        Self(bus)
    }
}

impl Drop for FlashBusy {
    fn drop(&mut self) {
        self.0.flash_busy.store(false, Ordering::Relaxed);
    }
}

/// Why an upload stopped.
#[derive(Debug)]
enum Refusal {
    Image(ImageError),
    Flash(firmware::FirmwareError),
}

/// Streams the body into the target slot, a sector at a time. The store is
/// locked per sector, not for the whole upload, so a schedule save is delayed
/// by one sector's erase at most.
#[allow(clippy::too_many_arguments, reason = "one call site, all of it wiring")]
async fn receive(
    socket: &mut TcpSocket<'_>,
    head: &Head,
    request: &mut [u8; http::REQUEST_LEN],
    filled: usize,
    check: &mut ImageCheck<'_>,
    sectors: &mut Sectors<SECTOR>,
    store: &'static SharedStore,
    target: Target,
) -> Result<(), Refusal> {
    // What came with the head first; then the request buffer is free to read
    // into.
    let first = filled.min(head.body_at + head.content_length);
    let mut got = first - head.body_at;
    take(&request[head.body_at..first], check, sectors, store, target).await?;

    while got < head.content_length {
        let want = (head.content_length - got).min(request.len());
        match socket.read(&mut request[..want]).await {
            Ok(0) | Err(_) => return Err(Refusal::Image(ImageError::Truncated)),
            Ok(n) => {
                got += n;
                take(&request[..n], check, sectors, store, target).await?;
            }
        }
    }

    if let Some((at, bytes)) = sectors.rest() {
        firmware::write_sector(&mut *store.lock().await, target, at, bytes)
            .map_err(Refusal::Flash)?;
    }
    Ok(())
}

/// Checks the bytes, then writes every sector they complete.
async fn take(
    mut bytes: &[u8],
    check: &mut ImageCheck<'_>,
    sectors: &mut Sectors<SECTOR>,
    store: &'static SharedStore,
    target: Target,
) -> Result<(), Refusal> {
    check.feed(bytes).map_err(Refusal::Image)?;
    while !bytes.is_empty() {
        let (took, full) = sectors.push(bytes);
        if let Some((at, sector)) = full {
            firmware::write_sector(&mut *store.lock().await, target, at, sector)
                .map_err(Refusal::Flash)?;
        }
        bytes = &bytes[took..];
    }
    Ok(())
}
