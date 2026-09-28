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
//! | `POST /schedule` | the meals, through the schedule task — no restart |
//! | `POST /clock` | a hand-set time, like the knob's `Clock` |
//! | `POST /calibration` | the detent and portion scale, into flash and in force |
//! | `POST /calibrate` | start a calibration run; the page follows it |
//! | `POST /network` | Wi-Fi and broker, into flash, then a restart |
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
    clock_from_form, feed_from_form, network_from_form, render_page, render_restarting,
    same_origin, schedule_from_form,
};
use crate::http;
use crate::provisioning::{Head, Method, PAGE_LEN};
use crate::schedule::ScheduleCommand;
use crate::store::SharedStore;
use crate::wiring::Bus;

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

        if let Some((head, body)) = http::read_request("web", slot, &mut socket, request).await {
            let restart = handle(slot, &mut socket, &head, body, page, store, unit, bus).await;
            if restart {
                // The browser is told before the unit disappears. Without the
                // flush the reset races the last segment and the browser shows
                // a connection error on what was actually a success.
                socket.close();
                let _ = socket.flush().await;
                Timer::after(Duration::from_millis(250)).await;
                info!("web: network saved, restarting");
                esp_hal::system::software_reset()
            }
        }

        http::reset(&mut socket).await;
    }
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
        (Method::Post, "/clock") => {
            let notice = match clock_from_form(body) {
                Ok(wall) => {
                    bus.set_clock_by_hand(wall);
                    info!("web: clock set by hand to {wall}");
                    Notice::Done("Clock set.")
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
