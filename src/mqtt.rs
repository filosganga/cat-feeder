//! MQTT: last will, Home Assistant discovery, command subscriptions and state.
//!
//! The connection order is fixed and getting it wrong makes entities appear
//! unavailable or not at all:
//!
//! 1. CONNECT carrying the will, so the broker says `offline` for us.
//! 2. The retained discovery configs — see `discovery.rs` for the entities.
//! 3. `online`, retained.
//! 4. Subscribe to the command topics.
//! 5. The retained `feeder/<id>/schedule/state` echo: what this unit holds, since the
//!    broker's copy may predate a reboot or a factory reset. A pause changed on
//!    the knob while offline goes out *before* step 4, so the replay that
//!    follows carries it back.
//! 6. Ask for the time — after the subscriptions, or the answer arrives before
//!    anything is listening for it.
//! 7. The first state — but only after the retained `paused` has had a chance
//!    to arrive, or Home Assistant briefly shows a paused feeder as running.
//!
//! Discovery is retained, so Home Assistant re-reads it after a restart on its
//! own and this firmware never subscribes to `homeassistant/status`.

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::num::NonZero;
use core::str::FromStr as _;

use embassy_futures::select::{Either4, select4};
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use heapless::String;
use log::{error, info, warn};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::AllocBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::event::Event;
use rust_mqtt::client::options::{
    ConnectOptions, PublicationOptions, SubscriptionOptions, TopicReference, WillOptions,
};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::io::Transport;
use rust_mqtt::types::{MqttBinary, MqttString, TopicFilter, TopicName};

use crate::config::Config;
use crate::discovery::{self, DISCOVERY_LEN, TOPIC_LEN};
use crate::schedule::{Schedule, ScheduleCommand, SlotEdit, TimeSource, Wall, parse_time};
use crate::wiring::{Bus, TimeSync, now_ms};

const PAYLOAD_LEN: usize = 128;
const CLIENT_ID_LEN: usize = 16;

const STATE_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// How long the TCP connect, and then the MQTT CONNECT/CONNACK, may each take.
///
/// Neither has a timeout of its own. A SYN into a dead link is retransmitted
/// indefinitely, so without this a unit on a bad Wi-Fi association sat on
/// `mqtt: connecting` for over 100 s — silent, and never reaching the retry
/// below. A healthy connect to a LAN broker takes well under a second, so ten
/// is generous, and the failure then lands in the ordinary 5 s retry loop.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to let retained messages land after subscribing, before publishing
/// a state payload that claims to know whether this unit is paused.
const RETAINED_GRACE: Duration = Duration::from_millis(1_000);

/// The broker declares a client dead after 1.5x this, and only then publishes
/// the will. That delay is how long a powered-off feeder still reads `online`
/// in Home Assistant, so it is kept short: 15s here means roughly 22s.
///
/// This is safe only because [`STATE_INTERVAL`] is shorter, so the state
/// publishes themselves keep the connection alive and no ping loop is needed.
/// **If state publishing ever becomes event-driven, add an explicit
/// `client.ping()` loop or this connection will be dropped when idle.**
const KEEP_ALIVE: KeepAlive = KeepAlive::Seconds(NonZero::new(15).unwrap());

const PAYLOAD_ONLINE: &str = "online";
const PAYLOAD_OFFLINE: &str = "offline";

/// Broadcast feed. No discovery entity: Home Assistant automations publish here
/// directly, and it is how three feeders feed at the same instant.
const TOPIC_ALL_FEED: &str = "feeder/all/feed";
/// Broadcast schedule: every unit stores it and puts it in force. A command,
/// **never retained** — see `on_schedule` — and the only way three feeders are
/// told to eat the same meals. There is deliberately no shared retained
/// schedule any more: a unit that joins is not handed meals nobody chose for
/// it, and starts blank.
const TOPIC_ALL_SCHEDULE: &str = "feeder/all/schedule";
const TOPIC_TIME: &str = "feeder/time";

/// Asks Home Assistant to publish `feeder/time` now. Carries this unit's id,
/// and is deliberately **not** retained: a retained request would be replayed
/// to Home Assistant on every one of its own restarts, which is the same rule
/// the two `feed` topics follow.
const TOPIC_TIME_REQUEST: &str = "feeder/time/request";

/// Shown in Home Assistant's device page, nowhere else.
const SW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Concrete client type, so helpers can name it without repeating the generics.
///
/// `MAX_SUBSCRIBES` is 8 because all seven SUBSCRIBE packets are sent before
/// any SUBACK is read; they are only removed from that list once the main loop
/// polls the acknowledgements. One more topic filter needs this raised.
type FeederClient<'c, N> = Client<'c, N, AllocBuffer, 8, 2, 2, 2>;

/// The per-unit topics. Built once, because every publish borrows from them.
struct Topics {
    availability: String<TOPIC_LEN>,
    state: String<TOPIC_LEN>,
    feed: String<TOPIC_LEN>,
    paused: String<TOPIC_LEN>,
    /// `feeder/<id>/schedule`: a schedule for this unit alone.
    schedule_cmd: String<TOPIC_LEN>,
    /// `feeder/<id>/schedule/state`: retained, what this unit holds.
    schedule_state: String<TOPIC_LEN>,
    /// `feeder/<id>/meal/`: the prefix of every `Meal n` entity's command.
    meal_prefix: String<TOPIC_LEN>,
    /// `feeder/<id>/meal/+/+`: all sixteen of them, in one subscription.
    meal_filter: String<TOPIC_LEN>,
}

impl Topics {
    /// Cannot truncate: the device id is six characters and [`TOPIC_LEN`] is
    /// sized for the longest topic this firmware builds.
    fn new(id: &str) -> Self {
        let mut availability = String::new();
        let _ = write!(availability, "feeder/{id}/availability");

        let mut state = String::new();
        let _ = write!(state, "feeder/{id}/state");

        let mut feed = String::new();
        let _ = write!(feed, "feeder/{id}/feed");

        let mut paused = String::new();
        let _ = write!(paused, "feeder/{id}/paused");

        let mut schedule_cmd = String::new();
        let _ = write!(schedule_cmd, "feeder/{id}/schedule");

        let mut schedule_state = String::new();
        let _ = write!(schedule_state, "feeder/{id}/schedule/state");

        let mut meal_prefix = String::new();
        let _ = write!(meal_prefix, "feeder/{id}/meal/");

        let mut meal_filter = String::new();
        let _ = write!(meal_filter, "feeder/{id}/meal/+/+");

        Self {
            availability,
            state,
            feed,
            paused,
            schedule_cmd,
            schedule_state,
            meal_prefix,
            meal_filter,
        }
    }
}

/// What the unit reports about itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    pub feeding: bool,
    pub jammed: bool,
    pub paused: bool,
    /// Meals a day in the schedule this unit holds, `0` for none. The one
    /// number that says a healthy-looking unit will never feed.
    pub meals: usize,
    /// The time and the portion count. Only the time reaches MQTT; the
    /// count is for the display, which reads the same slot.
    pub last_fed: Option<(Wall, u8)>,
}

impl State {
    fn read(bus: &Bus) -> Self {
        Self {
            feeding: bus.status.feeding(),
            jammed: bus.status.jammed(),
            paused: bus.is_paused(),
            meals: bus.held.meals(),
            last_fed: bus.last_fed.get(),
        }
    }

    /// `last_fed` is local wall-clock, carrying whatever offset Home Assistant
    /// published, and covers scheduled feeds only. It is informational; no
    /// entity reads it.
    fn to_json(self) -> String<PAYLOAD_LEN> {
        let mut json = String::new();
        let _ = write!(
            json,
            r#"{{"feeding":{},"jammed":{},"paused":{},"meals":{},"last_fed":"#,
            self.feeding, self.jammed, self.paused, self.meals
        );

        match self.last_fed {
            None => {
                let _ = write!(json, "null}}");
            }
            Some((at, _portions)) => {
                let _ = write!(json, r#""{at}"}}"#);
            }
        }

        json
    }
}

/// Connects to the broker and keeps publishing state, reconnecting forever.
///
/// Never returns: losing the broker is normal, not fatal. The unit keeps
/// running and retries.
pub async fn run(stack: Stack<'static>, cfg: Config, id: &str, bus: &'static Bus) -> ! {
    let topics = Topics::new(id);

    let mut client_id: String<CLIENT_ID_LEN> = String::new();
    let _ = write!(client_id, "feeder_{id}");

    // No DNS resolver in the firmware yet, so the broker is an address.
    let Ok(host) = Ipv4Addr::from_str(cfg.mqtt_host) else {
        error!(
            "mqtt: mqtt_host `{}` is not an IPv4 address; fix cfg.toml and rebuild",
            cfg.mqtt_host
        );
        loop {
            Timer::after(Duration::from_secs(60)).await;
        }
    };
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(host), cfg.mqtt_port);

    loop {
        if session(stack, cfg, endpoint, &topics, &client_id, id, bus)
            .await
            .is_err()
        {
            warn!("mqtt: disconnected, retrying in 5s");
        }

        // One place, covering every way out of a session — a failed TCP
        // connect, a rejected CONNECT, a read error mid-stream. Clearing it
        // inside `session` would mean finding all of them.
        bus.net.set_broker(false);

        Timer::after(RECONNECT_DELAY).await;
    }
}

/// One connection, from TCP connect until something fails.
#[allow(clippy::too_many_arguments, reason = "one call site, all of it wiring")]
async fn session(
    stack: Stack<'static>,
    cfg: Config,
    endpoint: IpEndpoint,
    topics: &Topics,
    client_id: &str,
    id: &str,
    bus: &'static Bus,
) -> Result<(), ()> {
    let mut rx_buffer = [0u8; 1024];
    let mut tx_buffer = [0u8; 1024];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

    info!("mqtt: connecting to {}:{}", cfg.mqtt_host, cfg.mqtt_port);
    match with_timeout(CONNECT_TIMEOUT, socket.connect(endpoint)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!("mqtt: tcp connect failed: {e:?}");
            return Err(());
        }
        Err(_) => {
            warn!(
                "mqtt: tcp connect timed out after {}s",
                CONNECT_TIMEOUT.as_secs()
            );
            return Err(());
        }
    }

    let availability = topic(&topics.availability)?;

    // The will is registered in the CONNECT packet, so the broker publishes
    // `offline` for us if this unit drops off without saying goodbye.
    let options = ConnectOptions::new()
        .clean_start()
        .keep_alive(KEEP_ALIVE)
        .user_name(string(cfg.mqtt_user)?)
        .password(binary(cfg.mqtt_password)?)
        .will(WillOptions::new(availability, binary(PAYLOAD_OFFLINE)?).retain());

    let mut buffer = AllocBuffer;
    let mut client = FeederClient::new(&mut buffer);

    // The broker accepted TCP but may never answer CONNECT — a half-dead link,
    // or a broker wedged under load. Same bound, same retry.
    match with_timeout(
        CONNECT_TIMEOUT,
        client.connect(socket, &options, Some(string(client_id)?)),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            warn!("mqtt: connect failed: {e:?}");
            return Err(());
        }
        Err(_) => {
            warn!("mqtt: no CONNACK within {}s", CONNECT_TIMEOUT.as_secs());
            return Err(());
        }
    }
    info!("mqtt: connected, id={client_id}");

    // TCP plus CONNACK, which is the honest meaning of "reaching the broker".
    // Cleared in `run` when this session ends, however it ends.
    bus.net.set_broker(true);

    // The address now, not the one at boot: a Wi-Fi reconnect can come back on
    // a new lease, and the discovery below carries it as Home Assistant's
    // *Visit device* link. The knob's `WI-FI` page reads the same value.
    if let Some(v4) = stack.config_v4() {
        bus.ip.set(Some(v4.address.address().octets()));
    }

    publish_discovery(&mut client, id, bus).await?;

    publish(
        &mut client,
        &topics.availability,
        PAYLOAD_ONLINE.as_bytes(),
        true,
    )
    .await?;
    info!("mqtt: online");

    // A pause chosen on the knob while the broker was unreachable. Published
    // **before** subscribing, so the retained replay that follows carries the
    // new value back rather than the stale one — which would otherwise undo
    // the change the moment the unit reconnected.
    if let Some(paused) = bus.pause_request.try_take() {
        publish_paused(&mut client, topics, paused).await?;
    }

    for filter in [
        topics.feed.as_str(),
        TOPIC_ALL_FEED,
        topics.paused.as_str(),
        topics.schedule_cmd.as_str(),
        TOPIC_ALL_SCHEDULE,
        topics.meal_filter.as_str(),
        TOPIC_TIME,
    ] {
        subscribe(&mut client, filter).await?;
    }
    info!("mqtt: subscribed");

    // What this unit holds, for Home Assistant and anyone else to read — on
    // every connect, because the broker's copy may be from before a reboot or
    // a factory reset.
    publish_schedule(&mut client, topics, bus).await?;

    // Home Assistant publishes the time once a minute and only a *live* one
    // arms the schedule, so a unit that connects just after a tick waits up to
    // a full minute with nothing to do. Asking collapses that wait; the answer
    // is an ordinary live publish and proves Home Assistant is running now,
    // which is the whole content of the live/retained distinction.
    //
    // **After the subscriptions, and that is the trick**: a reply that arrives
    // before the subscription exists is a reply nobody hears. Once per
    // connection and never on a timer — this exists to collapse the initial
    // wait, and a unit that keeps asking is a unit in a reconnect loop making
    // it worse.
    //
    // Nobody may answer, and that is fine: a unit whose request is ignored
    // simply waits for the next periodic publish, which is the old behaviour.
    publish(&mut client, TOPIC_TIME_REQUEST, id.as_bytes(), false).await?;
    info!("mqtt: asked for the time");

    // The retained `paused`, `schedule` and `time` arrive right after the
    // subscriptions. Hold the first state publish back until they have had
    // their moment, so the switch in Home Assistant never flickers.
    let mut next_state = Instant::now() + RETAINED_GRACE;

    loop {
        // `poll_header` is cancel-safe and `poll_body` is not, which is exactly
        // why the select waits on the header alone. Reading the body then runs
        // to completion with nothing racing it.
        //
        // `Signal::wait` is cancel-safe as well: it only takes the value when
        // it resolves, so losing the race to a header leaves it pending.
        let next = select4(
            client.poll_header(),
            Timer::at(next_state),
            bus.pause_request.wait(),
            bus.schedule_changed.wait(),
        )
        .await;

        match next {
            Either4::First(header) => {
                let header = header.map_err(|e| warn!("mqtt: poll failed: {e:?}"))?;
                let event = client
                    .poll_body(header)
                    .await
                    .map_err(|e| warn!("mqtt: read failed: {e:?}"))?;

                if let Event::Publish(message) = event
                    && on_message(
                        message.topic.as_ref().as_str(),
                        &message.message,
                        // True only for messages the broker replayed at
                        // subscribe time: the subscription leaves
                        // `retain_as_published` off, so the flag is cleared on
                        // everything forwarded live. `feeder/time` depends on
                        // that distinction.
                        message.retain,
                        topics,
                        bus,
                    )
                {
                    // Home Assistant's paused switch is not optimistic: it only
                    // moves once this state arrives. Do not make the user wait
                    // out the interval.
                    next_state = Instant::now();
                }
            }

            Either4::Fourth(()) => {
                publish_schedule(&mut client, topics, bus).await?;
                // `meals` in the state payload changed too.
                next_state = Instant::now();
            }

            Either4::Third(paused) => {
                // The broker echoes it straight back on the subscription, and
                // that echo is what `on_message` acts on and what brings the
                // state payload forward.
                publish_paused(&mut client, topics, paused).await?;
            }

            Either4::Second(()) => {
                next_state = Instant::now() + STATE_INTERVAL;

                let payload = State::read(bus).to_json();
                publish(&mut client, &topics.state, payload.as_bytes(), true).await?;
            }
        }
    }
}

/// Publishes the retained pause flag, as Home Assistant's switch would.
///
/// For a change made at the feeder itself. The retained topic is where the
/// flag lives — see `Bus::pause_request` — so this is the same write Home
/// Assistant makes, and its switch follows through the state payload.
async fn publish_paused<N: Transport>(
    client: &mut FeederClient<'_, N>,
    topics: &Topics,
    paused: bool,
) -> Result<(), ()> {
    let payload = if paused { "ON" } else { "OFF" };
    publish(client, &topics.paused, payload.as_bytes(), true).await?;
    info!("mqtt: published paused = {payload}, from the menu");
    Ok(())
}

/// Publishes what this unit holds, retained, on `feeder/<id>/schedule/state`. `[]`
/// for a unit with no schedule: an empty list and no list feed the same.
async fn publish_schedule<N: Transport>(
    client: &mut FeederClient<'_, N>,
    topics: &Topics,
    bus: &'static Bus,
) -> Result<(), ()> {
    let json = bus.held.get().unwrap_or_default().to_json();
    publish(client, &topics.schedule_state, json.as_bytes(), true).await
}

/// Acts on one incoming publication.
///
/// Returns true if the state payload should go out now rather than at the next
/// interval.
fn on_message(
    topic_name: &str,
    payload: &[u8],
    retained: bool,
    topics: &Topics,
    bus: &'static Bus,
) -> bool {
    if topic_name == topics.feed.as_str() || topic_name == TOPIC_ALL_FEED {
        on_feed(payload, bus);
        false
    } else if topic_name == topics.paused.as_str() {
        // Reported as the command that arrived, not as a transition: the
        // retained flag is replayed on every reconnect, so "resumed" would be
        // logged on a unit that was never paused.
        match payload {
            b"ON" => {
                let changed = !bus.is_paused();
                bus.set_paused(true);
                info!("mqtt: paused = ON");
                changed
            }
            b"OFF" => {
                let changed = bus.is_paused();
                bus.set_paused(false);
                info!("mqtt: paused = OFF");
                changed
            }
            _ => {
                warn!("mqtt: paused payload must be ON or OFF");
                false
            }
        }
    } else if topic_name == TOPIC_TIME {
        on_time(payload, retained, bus);
        false
    } else if topic_name == topics.schedule_cmd.as_str() || topic_name == TOPIC_ALL_SCHEDULE {
        on_schedule(payload, retained, bus);
        false
    } else if let Some(path) = topic_name.strip_prefix(topics.meal_prefix.as_str()) {
        on_meal_edit(path, payload, retained, bus);
        false
    } else {
        warn!("mqtt: unexpected topic {topic_name}");
        false
    }
}

/// Hands the time to the schedule task, stamped with the monotonic reading now.
///
/// The retained-or-live distinction travels with it. A retained `feeder/time`
/// is whatever the broker last stored, which is under a minute old while Home
/// Assistant is publishing and arbitrarily old once it stops — and the unit
/// cannot tell those apart by looking at the timestamp.
///
/// A payload that will not parse is dropped with a warning rather than stopping
/// the clock: the unit keeps free-running on the last time it understood, which
/// is the whole reason it keeps one.
fn on_time(payload: &[u8], retained: bool, bus: &'static Bus) {
    let monotonic_ms = now_ms();
    let source = if retained {
        TimeSource::Retained
    } else {
        TimeSource::Live
    };

    match parse_time(payload) {
        Ok(wall) => {
            let sync = TimeSync {
                monotonic_ms,
                wall,
                source,
            };
            if source == TimeSource::Live {
                bus.rtc_time.signal(sync);
            }
            bus.time.signal(sync);
        }
        Err(e) => warn!("mqtt: time payload rejected: {e:?}"),
    }
}

/// Hands a schedule command to the schedule task, which stores it.
///
/// A rejected payload leaves the previous schedule in place. That is
/// deliberate: a unit running yesterday's schedule feeds the cats, and a unit
/// with no schedule does not.
///
/// **A retained command is refused.** These are commands, and a retained one
/// would be handed to every unit that subscribes later — which is exactly the
/// inheritance the unit owning its schedule exists to end: a board on the
/// bench pointed at the house broker would start feeding meals nobody chose
/// for it. So the retain flag, which the broker sets only on a replay, means
/// "not addressed to you now", and is ignored with a line saying so.
fn on_schedule(payload: &[u8], retained: bool, bus: &'static Bus) {
    // An empty payload is how a retained message is deleted from the broker,
    // and a unit subscribed at the time receives the deletion too. It is
    // nobody's command, so it is not reported as a malformed one.
    if payload.is_empty() {
        return;
    }
    if retained {
        warn!("mqtt: ignored a retained schedule command; publish it without retain");
        return;
    }
    match Schedule::parse(payload) {
        Ok(schedule) => send_schedule(ScheduleCommand::Replace(schedule), bus),
        Err(e) => warn!("mqtt: schedule payload rejected: {e:?}, keeping the last one"),
    }
}

/// One slot changed from a `Meal n` entity in Home Assistant.
///
/// Same rules as a whole schedule: an empty payload is a retained message
/// being deleted, and a retained edit is refused — a unit subscribing later
/// must not inherit somebody else's meal times. Whether the edit *applies* is
/// the schedule task's call, since it holds the schedule it edits.
fn on_meal_edit(path: &str, payload: &[u8], retained: bool, bus: &'static Bus) {
    if payload.is_empty() {
        return;
    }
    if retained {
        warn!("mqtt: ignored a retained meal edit on {path}; publish it without retain");
        return;
    }
    match SlotEdit::parse(path, payload) {
        Ok(edit) => send_schedule(ScheduleCommand::Edit(edit), bus),
        Err(e) => warn!("mqtt: meal edit on {path} rejected: {e:?}"),
    }
}

fn send_schedule(command: ScheduleCommand, bus: &'static Bus) {
    if bus.schedule.try_send(command).is_err() {
        warn!("mqtt: schedule queue full, command dropped");
    }
}

/// Manual feeds accumulate: this forwards the count and never replaces it.
///
/// The clamp to `MAX_CLICKS` lives in the feeder, which is the only place
/// that knows how much is already pending.
fn on_feed(payload: &[u8], bus: &'static Bus) {
    let Some(portions) = core::str::from_utf8(payload)
        .ok()
        .and_then(|text| text.trim().parse::<u8>().ok())
    else {
        warn!("mqtt: feed payload is not a portion count");
        return;
    };

    if portions == 0 {
        // A no-op, not an error.
        info!("mqtt: feed 0 ignored");
        return;
    }

    // Manual feed works while paused, on purpose. Pause stops the schedule, not
    // the feeder, so a bowl can always be topped up by hand.
    match bus.feed.try_send(portions) {
        Ok(()) => info!("mqtt: feed {portions}"),
        Err(_) => warn!("mqtt: feed queue full, {portions} portions dropped"),
    }
}

/// Every entity `discovery.rs` names, each retained so Home Assistant re-reads
/// them by itself.
async fn publish_discovery<N: Transport>(
    client: &mut FeederClient<'_, N>,
    id: &str,
    bus: &'static Bus,
) -> Result<(), ()> {
    // One pair of buffers, reused, because the whole connection's future is
    // sized by whatever is live at an await point.
    let mut topic_name: String<TOPIC_LEN> = String::new();
    let mut payload: String<DISCOVERY_LEN> = String::new();

    for entity in discovery::entities() {
        // A truncated config is a classic reason an entity never appears, so
        // one that did not fit is not published at all.
        entity
            .render(id, SW_VERSION, bus.ip.get(), &mut topic_name, &mut payload)
            .map_err(|_| error!("mqtt: discovery for {entity:?} does not fit its buffer"))?;
        publish(client, &topic_name, payload.as_bytes(), true).await?;
    }

    info!("mqtt: discovery published");
    Ok(())
}

async fn subscribe<N: Transport>(
    client: &mut FeederClient<'_, N>,
    topic_filter: &str,
) -> Result<(), ()> {
    let filter = TopicFilter::new(string(topic_filter)?).ok_or_else(|| {
        error!("mqtt: `{topic_filter}` is not a valid topic filter");
    })?;

    // At most once would be enough for `feed`, but the retained `paused`,
    // `schedule` and `time` are worth a PUBACK. The duplicate a QoS 1 redelivery
    // can cause is what `MAX_CLICKS` guards against.
    let options = SubscriptionOptions::new().at_least_once();

    client
        .subscribe(filter, options)
        .await
        .map(|_| ())
        .map_err(|e| warn!("mqtt: subscribe to {topic_filter} failed: {e:?}"))
}

async fn publish<N: Transport>(
    client: &mut FeederClient<'_, N>,
    topic_name: &str,
    payload: &[u8],
    retain: bool,
) -> Result<(), ()> {
    let mut options = PublicationOptions::new(TopicReference::Name(topic(topic_name)?));
    if retain {
        options = options.retain();
    }

    match client.publish(&options, Bytes::from(payload)).await {
        Ok(_) => Ok(()),
        Err(e) => {
            warn!("mqtt: publish to {topic_name} failed: {e:?}");
            Err(())
        }
    }
}

fn string(text: &str) -> Result<MqttString<'_>, ()> {
    MqttString::try_from(text).map_err(|_| {
        error!("mqtt: `{text}` is not a valid MQTT string");
    })
}

fn binary(text: &str) -> Result<MqttBinary<'_>, ()> {
    MqttBinary::try_from(text).map_err(|_| {
        error!("mqtt: payload too long");
    })
}

fn topic(name: &str) -> Result<TopicName<'_>, ()> {
    TopicName::new(string(name)?).ok_or_else(|| {
        error!("mqtt: `{name}` is not a valid topic name");
    })
}
