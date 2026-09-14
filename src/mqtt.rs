//! MQTT connection: last will, availability and state publishing.
//!
//! This is the first increment of roadmap step 4. The state it publishes is
//! mocked; the feeder task that will own the real values does not exist yet.
//! Discovery and command subscriptions come next.

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::num::NonZero;
use core::str::FromStr as _;

use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_time::{Duration, Timer};
use heapless::String;
use log::{error, info, warn};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::AllocBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{ConnectOptions, PublicationOptions, TopicReference, WillOptions};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::io::Transport;
use rust_mqtt::types::{MqttBinary, MqttString, TopicName};

use crate::config::Config;

/// Longest topic this firmware builds, `feeder/<id>/availability`.
const TOPIC_LEN: usize = 48;
const PAYLOAD_LEN: usize = 128;
const CLIENT_ID_LEN: usize = 16;

const STATE_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

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

/// Concrete client type, so helpers can name it without repeating the generics.
type FeederClient<'c, N> = Client<'c, N, AllocBuffer, 2, 2, 2, 2>;

/// The topics this unit publishes to. Built once, because every publish
/// borrows from them.
struct Topics {
    availability: String<TOPIC_LEN>,
    state: String<TOPIC_LEN>,
}

impl Topics {
    fn new(id: &str) -> Self {
        let mut availability = String::new();
        let _ = write!(availability, "feeder/{id}/availability");

        let mut state = String::new();
        let _ = write!(state, "feeder/{id}/state");

        Self {
            availability,
            state,
        }
    }
}

/// What the unit reports about itself.
///
/// Mocked for now. When the feeder task lands it owns these values and this
/// struct arrives over a channel instead of being invented here.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    pub feeding: bool,
    pub jammed: bool,
    pub paused: bool,
}

impl State {
    fn to_json(self) -> String<PAYLOAD_LEN> {
        let mut json = String::new();
        let _ = write!(
            json,
            r#"{{"feeding":{},"jammed":{},"paused":{},"last_fed":null}}"#,
            self.feeding, self.jammed, self.paused
        );
        json
    }
}

/// Connects to the broker and keeps publishing state, reconnecting forever.
///
/// Never returns: losing the broker is normal, not fatal. The unit keeps
/// running and retries.
pub async fn run(stack: Stack<'static>, cfg: Config, id: &str) -> ! {
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
        if session(stack, cfg, endpoint, &topics, &client_id)
            .await
            .is_err()
        {
            warn!("mqtt: disconnected, retrying in 5s");
        }
        Timer::after(RECONNECT_DELAY).await;
    }
}

/// One connection, from TCP connect until something fails.
async fn session(
    stack: Stack<'static>,
    cfg: Config,
    endpoint: IpEndpoint,
    topics: &Topics,
    client_id: &str,
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

    publish(
        &mut client,
        &topics.availability,
        PAYLOAD_ONLINE.as_bytes(),
        true,
    )
    .await?;
    info!("mqtt: online");

    let mut state = State::default();
    let mut tick: u32 = 0;

    loop {
        // Mocked activity, so there is something to watch change in Home
        // Assistant: pretend to feed for one interval in every six.
        tick = tick.wrapping_add(1);
        state.feeding = tick.is_multiple_of(6);

        let payload = state.to_json();
        publish(&mut client, &topics.state, payload.as_bytes(), true).await?;
        info!("mqtt: state published {payload}");

        Timer::after(STATE_INTERVAL).await;
    }
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
