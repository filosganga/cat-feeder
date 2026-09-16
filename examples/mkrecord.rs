//! Builds a provisioning record on the host, for `dev/provision.sh` to flash.
//!
//! ```sh
//! cargo run --example mkrecord --target "$(rustc -vV | awk '/^host:/{print $2}')" \
//!   -- --out record.bin --detent-ms 900
//! ```
//!
//! ## Why this exists
//!
//! Credentials compiled into the firmware are the thing roadmap step 9 is
//! removing. The alternative is not "a different build" but **no build at
//! all**: `provisioning::Record::encode` is pure and host-tested, so the same
//! code the firmware reads with can produce the bytes here, and `espflash`
//! writes them straight into the `nvs` partition — the one partition an
//! application reflash never touches.
//!
//! One consequence is a *faster* dev loop than compiled-in credentials gave:
//! provision a board once and it keeps its settings across every `cargo run`,
//! with nothing re-seeded at boot. Another is that the three production units
//! can be set up without ever raising an access point or typing on a phone.
//!
//! It is an example rather than a second `[[bin]]` because `[[bin]]` is the
//! firmware and is built for the ESP32-C6. Examples build for the host, which
//! is what lets this share `provisioning.rs` instead of reimplementing the
//! format — and sharing it is the whole point, since a second implementation
//! that drifted would write records the firmware rejects.

use std::process::ExitCode;

use cat_feeder::portions::SCALE_UNCHANGED;
use cat_feeder::provisioning::{DEFAULT_DETENT_MS, MAX_RECORD_LEN, MIN_DETENT_MS, Record};

const USAGE: &str = "\
usage: mkrecord [options]

  --config <path>        default: cfg.toml
  --out <path>           default: record.bin
  --detent-ms <ms>       override the measured detent interval
  --portion-scale <pct>  override the portion scale, 100 = unchanged

Values not overridden come from the config file. Credentials are read from it
and never compiled into anything.
";

fn main() -> ExitCode {
    match run() {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("mkrecord: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let args = Args::parse(std::env::args().skip(1))?;

    let text = std::fs::read_to_string(&args.config)
        .map_err(|e| format!("cannot read {}: {e}", args.config))?;
    let config: toml::Table = text
        .parse()
        .map_err(|e| format!("cannot parse {}: {e}", args.config))?;

    let record = build(&config, &args)?;

    // The full buffer, padded with the erased-flash pattern rather than left
    // short. `Store::load` reads MAX_RECORD_LEN and decodes from that, so
    // writing only the used bytes would leave whatever was in flash before
    // sitting after the record. It decodes either way — the length is in the
    // fields — but an image that is byte-for-byte the same every time is worth
    // more than the few bytes saved.
    let mut buffer = [0xFFu8; MAX_RECORD_LEN];
    let used = record
        .encode(&mut buffer)
        .map_err(|e| format!("cannot encode the record: {e:?}"))?;

    std::fs::write(&args.out, buffer).map_err(|e| format!("cannot write {}: {e}", args.out))?;

    Ok(format!(
        "{}: {} bytes ({used} used)\n  wifi   {}\n  broker {}:{}\n  detent {} ms\n  scale  {}%",
        args.out,
        MAX_RECORD_LEN,
        record.wifi_ssid,
        record.mqtt_host,
        record.mqtt_port,
        record.detent_ms,
        record.portion_scale_pct,
    ))
}

fn build(config: &toml::Table, args: &Args) -> Result<Record, String> {
    let detent_ms = args
        .detent_ms
        .or_else(|| number(config, "detent_ms"))
        .unwrap_or(DEFAULT_DETENT_MS);

    // Rejected here rather than clamped silently. The firmware clamps because a
    // bad value must not brick a unit in the field; this is a person at a bench
    // who can be told, and a typo they never see is a feeder that jams.
    if detent_ms < MIN_DETENT_MS {
        return Err(format!(
            "detent_ms {detent_ms} is below the {MIN_DETENT_MS} ms floor; \
             the derived click spacing would start rejecting real clicks"
        ));
    }

    let portion_scale_pct = args
        .portion_scale
        .or_else(|| number(config, "portion_scale_pct"))
        .unwrap_or(SCALE_UNCHANGED);

    if portion_scale_pct == 0 {
        return Err("portion_scale_pct 0 would round every meal to one click".into());
    }

    let record = Record {
        wifi_ssid: text(config, "wifi_ssid")?,
        wifi_password: optional_text(config, "wifi_password")?,
        mqtt_host: text(config, "mqtt_host")?,
        mqtt_port: number(config, "mqtt_port").unwrap_or(1883),
        mqtt_user: optional_text(config, "mqtt_user")?,
        mqtt_password: optional_text(config, "mqtt_password")?,
        detent_ms,
        portion_scale_pct,
    };

    // The same check the firmware makes before trying to connect, run here so a
    // hopeless record is caught at a keyboard instead of by a unit sitting in a
    // cupboard failing to join a network that does not exist.
    if !record.is_usable() {
        return Err("wifi_ssid, mqtt_host and mqtt_port must all be set".into());
    }

    Ok(record)
}

fn text<const N: usize>(config: &toml::Table, key: &str) -> Result<heapless::String<N>, String> {
    let value = config
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("{key} is missing"))?;

    heapless::String::try_from(value).map_err(|_| {
        format!(
            "{key} is {} bytes, which is more than the {N} the record holds",
            value.len()
        )
    })
}

fn optional_text<const N: usize>(
    config: &toml::Table,
    key: &str,
) -> Result<heapless::String<N>, String> {
    match config.get(key) {
        None => Ok(heapless::String::new()),
        Some(_) => text(config, key),
    }
}

fn number(config: &toml::Table, key: &str) -> Option<u16> {
    config
        .get(key)
        .and_then(toml::Value::as_integer)
        .and_then(|n| u16::try_from(n).ok())
}

struct Args {
    config: String,
    out: String,
    detent_ms: Option<u16>,
    portion_scale: Option<u16>,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Self {
            config: "cfg.toml".into(),
            out: "record.bin".into(),
            detent_ms: None,
            portion_scale: None,
        };

        let mut argv = argv.peekable();
        while let Some(flag) = argv.next() {
            let mut value = || {
                argv.next()
                    .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
            };

            match flag.as_str() {
                "--config" => args.config = value()?,
                "--out" => args.out = value()?,
                "--detent-ms" => args.detent_ms = Some(parse_u16(&value()?)?),
                "--portion-scale" => args.portion_scale = Some(parse_u16(&value()?)?),
                "-h" | "--help" => return Err(USAGE.into()),
                other => return Err(format!("unknown option {other}\n\n{USAGE}")),
            }
        }

        Ok(args)
    }
}

fn parse_u16(raw: &str) -> Result<u16, String> {
    raw.parse()
        .map_err(|_| format!("{raw} is not a number between 0 and {}", u16::MAX))
}
