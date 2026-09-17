//! MQTT: last will, Home Assistant discovery, command subscriptions and state.
//!
//! The connection order is fixed and getting it wrong makes entities appear
//! unavailable or not at all:
//!
//! 1. CONNECT carrying the will, so the broker says `offline` for us.
//! 2. The three retained discovery configs.
//! 3. `online`, retained.
//! 4. Subscribe to the command topics.
//! 5. The first state — but only after the retained `paused` has had a chance
//!    to arrive, or Home Assistant briefly shows a paused feeder as running.
//!
//! Discovery is retained, so Home Assistant re-reads it after a restart on its
//! own and this firmware never subscribes to `homeassistant/status`.

use core::fmt::{Arguments, Write as _};
use core::net::Ipv4Addr;
use core::num::NonZero;
use core::str::FromStr as _;

use embassy_futures::select::{Either, select};
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_time::{Duration, Instant, Timer};
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
use crate::schedule::{Schedule, TimeSource, Wall, parse_time};
use crate::wiring::{Bus, TimeSync, now_ms};

/// Longest topic this firmware builds is a discovery config,
/// `homeassistant/binary_sensor/feeder_<id>/jammed/config`, 55 characters.
const TOPIC_LEN: usize = 64;
const PAYLOAD_LEN: usize = 128;
/// Discovery payloads run to about 420 bytes with the device block. A silently
/// truncated one is a classic reason an entity never appears, so [`fmt_into`]
/// refuses to publish a payload that did not fit.
const DISCOVERY_LEN: usize = 512;
const CLIENT_ID_LEN: usize = 16;

const STATE_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

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
const TOPIC_SCHEDULE: &str = "feeder/schedule";
const TOPIC_TIME: &str = "feeder/time";

/// Shown in Home Assistant's device page, nowhere else.
const SW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Concrete client type, so helpers can name it without repeating the generics.
///
/// `MAX_SUBSCRIBES` is 8 because all five SUBSCRIBE packets are sent before any
/// SUBACK is read; they are only removed from that list once the main loop
/// polls the acknowledgements.
type FeederClient<'c, N> = Client<'c, N, AllocBuffer, 8, 2, 2, 2>;

/// The per-unit topics. Built once, because every publish borrows from them.
struct Topics {
    availability: String<TOPIC_LEN>,
    state: String<TOPIC_LEN>,
    feed: String<TOPIC_LEN>,
    paused: String<TOPIC_LEN>,
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

        Self {
            availability,
            state,
            feed,
            paused,
        }
    }
}

/// What the unit reports about itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    pub feeding: bool,
    pub jammed: bool,
    pub paused: bool,
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
            last_fed: bus.last_fed.get(),
        }
    }

    /// `last_fed` is local wall-clock, carrying whatever offset Home Assistant
    /// published, and covers scheduled feeds only. It is informational; none of
    /// the three entities reads it.
    fn to_json(self) -> String<PAYLOAD_LEN> {
        let mut json = String::new();
        let _ = write!(
            json,
            r#"{{"feeding":{},"jammed":{},"paused":{},"last_fed":"#,
            self.feeding, self.jammed, self.paused
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
    if let Err(e) = socket.connect(endpoint).await {
        warn!("mqtt: tcp connect failed: {e:?}");
        return Err(());
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

    if let Err(e) = client
        .connect(socket, &options, Some(string(client_id)?))
        .await
    {
        warn!("mqtt: connect failed: {e:?}");
        return Err(());
    }
    info!("mqtt: connected, id={client_id}");

    // TCP plus CONNACK, which is the honest meaning of "reaching the broker".
    // Cleared in `run` when this session ends, however it ends.
    bus.net.set_broker(true);

    publish_discovery(&mut client, id).await?;

    publish(
        &mut client,
        &topics.availability,
        PAYLOAD_ONLINE.as_bytes(),
        true,
    )
    .await?;
    info!("mqtt: online");

    for filter in [
        topics.feed.as_str(),
        TOPIC_ALL_FEED,
        topics.paused.as_str(),
        TOPIC_SCHEDULE,
        TOPIC_TIME,
    ] {
        subscribe(&mut client, filter).await?;
    }
    info!("mqtt: subscribed");

    // The retained `paused`, `schedule` and `time` arrive right after the
    // subscriptions. Hold the first state publish back until they have had
    // their moment, so the switch in Home Assistant never flickers.
    let mut next_state = Instant::now() + RETAINED_GRACE;

    loop {
        // `poll_header` is cancel-safe and `poll_body` is not, which is exactly
        // why the select waits on the header alone. Reading the body then runs
        // to completion with nothing racing it.
        let next = select(client.poll_header(), Timer::at(next_state)).await;

        match next {
            Either::First(header) => {
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

            Either::Second(()) => {
                next_state = Instant::now() + STATE_INTERVAL;

                let payload = State::read(bus).to_json();
                publish(&mut client, &topics.state, payload.as_bytes(), true).await?;
            }
        }
    }
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
    } else if topic_name == TOPIC_SCHEDULE {
        on_schedule(payload, bus);
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
        Ok(wall) => bus.time.signal(TimeSync {
            monotonic_ms,
            wall,
            source,
        }),
        Err(e) => warn!("mqtt: time payload rejected: {e:?}"),
    }
}

/// Hands the schedule to the schedule task.
///
/// A rejected payload leaves the previous schedule in place. That is
/// deliberate: a unit running yesterday's schedule feeds the cats, and a unit
/// with no schedule does not.
fn on_schedule(payload: &[u8], bus: &'static Bus) {
    match Schedule::parse(payload) {
        Ok(schedule) => bus.schedule.signal(schedule),
        Err(e) => warn!("mqtt: schedule payload rejected: {e:?}, keeping the last one"),
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

/// The three entities, each retained so Home Assistant re-reads them by itself.
///
/// All three carry the same `device` block and a `unique_id`, which is what
/// groups them into one device; without `unique_id` the device block is ignored
/// and the entities appear loose.
async fn publish_discovery<N: Transport>(
    client: &mut FeederClient<'_, N>,
    id: &str,
) -> Result<(), ()> {
    /// Repeated verbatim in all three payloads. `identifiers` is what joins
    /// them; the rest is cosmetic.
    macro_rules! device {
        () => {
            concat!(
                r#""device":{{"identifiers":["feeder_{id}"],"name":"Cat feeder {id}","#,
                r#""manufacturer":"DIY","model":"cat-feeder ESP32-C6","sw_version":"{sw}"}}"#,
            )
        };
    }

    // One buffer, reused, because the whole connection's future is sized by
    // whatever is live at an await point.
    let mut payload: String<DISCOVERY_LEN> = String::new();

    // `payload_press` is 1 and stays 1: three portions is three presses, which
    // the feeder accumulates. Deliberately not retained — a retained feed
    // command is replayed on every reconnect, and a boot loop would then empty
    // the hopper.
    fmt_into(
        &mut payload,
        format_args!(
            concat!(
                r#"{{"name":"Feed","unique_id":"feeder_{id}_feed","#,
                r#""command_topic":"feeder/{id}/feed","payload_press":"1","#,
                r#""availability_topic":"feeder/{id}/availability","#,
                device!(),
                "}}",
            ),
            id = id,
            sw = SW_VERSION
        ),
    )?;
    publish_config(client, "button", "feed", id, &payload).await?;

    // `"retain": true` makes Home Assistant publish the command retained, which
    // is the whole persistence story for pause: nothing is kept in flash, so a
    // unit that reboots comes back paused only because the broker remembers.
    //
    // Not optimistic: the switch moves when the unit echoes `paused` in its
    // state, so a switch that springs back means the command never landed.
    fmt_into(
        &mut payload,
        format_args!(
            concat!(
                r#"{{"name":"Paused","unique_id":"feeder_{id}_paused","#,
                r#""command_topic":"feeder/{id}/paused","state_topic":"feeder/{id}/state","#,
                r#""value_template":"{{{{ 'ON' if value_json.paused else 'OFF' }}}}","#,
                r#""retain":true,"optimistic":false,"#,
                r#""availability_topic":"feeder/{id}/availability","#,
                device!(),
                "}}",
            ),
            id = id,
            sw = SW_VERSION
        ),
    )?;
    publish_config(client, "switch", "paused", id, &payload).await?;

    // The template must not be `{{ value_json.jammed }}`: a JSON `true` renders
    // as Python's `True`, which matches neither `payload_on` nor `payload_off`,
    // and the entity sticks at unknown.
    fmt_into(
        &mut payload,
        format_args!(
            concat!(
                r#"{{"name":"Jammed","unique_id":"feeder_{id}_jammed","#,
                r#""state_topic":"feeder/{id}/state","#,
                r#""value_template":"{{{{ 'ON' if value_json.jammed else 'OFF' }}}}","#,
                r#""device_class":"problem","entity_category":"diagnostic","#,
                r#""availability_topic":"feeder/{id}/availability","#,
                device!(),
                "}}",
            ),
            id = id,
            sw = SW_VERSION
        ),
    )?;
    publish_config(client, "binary_sensor", "jammed", id, &payload).await?;

    info!("mqtt: discovery published");
    Ok(())
}

async fn publish_config<N: Transport>(
    client: &mut FeederClient<'_, N>,
    component: &str,
    object: &str,
    id: &str,
    payload: &str,
) -> Result<(), ()> {
    let mut topic_name: String<TOPIC_LEN> = String::new();
    fmt_into(
        &mut topic_name,
        format_args!("homeassistant/{component}/feeder_{id}/{object}/config"),
    )?;

    publish(client, &topic_name, payload.as_bytes(), true).await
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

/// Formats into a fixed buffer and fails loudly if it did not fit.
///
/// `write!` into a `heapless::String` truncates, and a truncated discovery
/// payload is one of the classic reasons an entity never appears in Home
/// Assistant. Silence is the wrong failure here.
fn fmt_into<const N: usize>(buffer: &mut String<N>, args: Arguments) -> Result<(), ()> {
    buffer.clear();
    buffer.write_fmt(args).map_err(|_| {
        error!("mqtt: {N} byte buffer too small, payload would be truncated");
    })
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
