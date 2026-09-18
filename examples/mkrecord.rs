//! Builds a provisioning record on the host, for `dev/provision.sh` to flash.
//!
//! ```sh
//! cargo run --example mkrecord --target "$(rustc -vV | awk '/^host:/{print $2}')" \
//!   -- --out record.bin --host 192.0.2.10 --detent-ms 900
//! ```
//!
//! `--host` is spelled out because `cfg.toml` ships `mqtt_host = "auto"`, which
//! only `dev/provision.sh` knows how to resolve — it means *the provisioning
//! machine's* address. Run directly, `auto` is rejected rather than stored,
//! since the firmware parses the field with `Ipv4Addr::from_str` and would
//! retry a connection it can never make.
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

  --config <path>         default: cfg.toml
  --out <path>            default: record.bin
  --host <ip>             override cfg.toml's mqtt_host, as a literal IPv4
  --user <name>           override cfg.toml's mqtt_user
  --password <secret>     override cfg.toml's mqtt_password
  --password-file <path>  ...read it from a file instead, or `-` for stdin
  --detent-ms <ms>        override the measured detent interval
  --portion-scale <pct>   override the portion scale, 100 = unchanged

Values not overridden come from the config file. Credentials are read from it
and never compiled into anything.

--password puts the secret in the process list and in shell history. For a
broker that matters, use --password-file, or `--password-file -` and pipe it.
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

    // The broker user is named here and the password is not. `--user` exists
    // because a different broker means different credentials, and getting it
    // wrong fails the same way a broker being down does — red ×2, a unit that
    // joined the Wi-Fi and was refused. Printing it back is the cheapest thing
    // that separates the two, and it matters most in the case this option was
    // added for: `MQTT_USER` exported for the dev stack, silently pairing a
    // dev username with a production password.
    Ok(format!(
        "{}: {} bytes ({used} used)\n  wifi   {}\n  broker {}@{}:{}\n  detent {} ms\n  scale  {}%",
        args.out,
        MAX_RECORD_LEN,
        record.wifi_ssid,
        if record.mqtt_user.is_empty() {
            "<no user>"
        } else {
            record.mqtt_user.as_str()
        },
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
        mqtt_host: match &args.host {
            Some(host) => host
                .as_str()
                .try_into()
                .map_err(|_| format!("--host {host} is longer than the record allows"))?,
            None => text(config, "mqtt_host")?,
        },
        mqtt_port: number(config, "mqtt_port").unwrap_or(1883),
        mqtt_user: match &args.user {
            Some(user) => user
                .as_str()
                .try_into()
                .map_err(|_| format!("--user {user} is longer than the record allows"))?,
            None => optional_text(config, "mqtt_user")?,
        },
        mqtt_password: match &args.password {
            // Deliberately not echoed on failure, unlike --user and --host: a
            // length complaint does not need the secret in it.
            Some(password) => password
                .as_str()
                .try_into()
                .map_err(|_| "the password is longer than the record allows".to_string())?,
            None => optional_text(config, "mqtt_password")?,
        },
        detent_ms,
        portion_scale_pct,
    };

    // The same check the firmware makes before trying to connect, run here so a
    // hopeless record is caught at a keyboard instead of by a unit sitting in a
    // cupboard failing to join a network that does not exist.
    if !record.is_usable() {
        return Err("wifi_ssid, mqtt_host and mqtt_port must all be set".into());
    }

    // And that the broker address is one the firmware can actually parse.
    // `mqtt.rs` reads it with `Ipv4Addr::from_str` and there is no resolver on
    // the device, so a hostname — or the literal `auto`, when this is run
    // directly rather than through `provision.sh`, which resolves it — stores
    // fine, survives the reboot, and leaves a unit retrying a connection it can
    // never make. `is_usable` cannot catch it: the field is set, just useless.
    if record.mqtt_host.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(format!(
            "mqtt_host is {:?}, which is not a literal IPv4 address.\n\
             The firmware has no resolver, so a name can never connect.\n\
             Pass --host, or write an address into the config file.",
            record.mqtt_host.as_str()
        ));
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
    host: Option<String>,
    user: Option<String>,
    password: Option<String>,
    detent_ms: Option<u16>,
    portion_scale: Option<u16>,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Self {
            config: "cfg.toml".into(),
            out: "record.bin".into(),
            host: None,
            user: None,
            password: None,
            detent_ms: None,
            portion_scale: None,
        };
        let mut password_from: Option<String> = None;

        let mut argv = argv.peekable();
        while let Some(flag) = argv.next() {
            let mut value = || {
                argv.next()
                    .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
            };

            match flag.as_str() {
                "--config" => args.config = value()?,
                "--out" => args.out = value()?,
                "--host" => args.host = Some(value()?),
                "--user" => args.user = Some(value()?),
                // Both spellings set the same field, so refusing here is what
                // stops one silently winning over the other. Whichever
                // precedence were chosen, the loser would be a password that
                // looks set and is not — and the symptom is a unit that joins
                // the network and is refused by the broker.
                //
                // `password_from` remembers which spelling arrived so a repeat
                // complains about the flag that was actually typed, rather than
                // naming one the user never used.
                "--password" | "--password-file" => {
                    let spelling = flag.clone();
                    if let Some(first) = &password_from {
                        return Err(if *first == spelling {
                            format!("{spelling} given twice\n\n{USAGE}")
                        } else {
                            format!(
                                "--password and --password-file are alternatives; pass one\n\n{USAGE}"
                            )
                        });
                    }
                    let raw = value()?;
                    args.password = Some(if spelling == "--password-file" {
                        read_password(&raw)?
                    } else {
                        raw
                    });
                    password_from = Some(spelling);
                }
                "--detent-ms" => args.detent_ms = Some(parse_u16(&value()?)?),
                "--portion-scale" => args.portion_scale = Some(parse_u16(&value()?)?),
                "-h" | "--help" => return Err(USAGE.into()),
                other => return Err(format!("unknown option {other}\n\n{USAGE}")),
            }
        }

        Ok(args)
    }
}

/// Reads a password from a file, or from stdin when the path is `-`.
///
/// **The first line is the password**, with a trailing `\r` dropped so a file
/// with CRLF endings works. Nothing else is stripped: a password may
/// legitimately begin or end with a space, and `provisioning::trimmed` draws
/// exactly this line already, trimming the SSID, host, port and username but
/// never either password.
///
/// First line rather than "everything but the trailing newline", for two
/// reasons that both come from how these files are actually made:
///
/// - `pass show mqtt/feeder` — the pipeline `dev/provision.sh` advertises —
///   prints the password on line one and metadata beneath it. Taking the whole
///   input would store the metadata too, and the unit would be refused by the
///   broker with nothing on the console to say why.
/// - `dev/watch.sh` reads the same file with shell `read`, which stops at the
///   first newline. The two have to agree, or the pre-flight check and the
///   provisioning disagree about the password — and the check is the thing
///   that is supposed to catch a wrong one *before* it reaches a unit.
///
/// A password containing a newline is therefore unrepresentable here. That is
/// a fair trade: it cannot be typed into the setup form either, and no broker
/// this talks to would accept one.
///
/// `-` for stdin is the only route that keeps the secret out of both the
/// process list and the filesystem.
fn read_password(path: &str) -> Result<String, String> {
    let raw = if path == "-" {
        std::io::read_to_string(std::io::stdin())
            .map_err(|e| format!("could not read the password from stdin: {e}"))?
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| format!("could not read the password from {path}: {e}"))?
    };

    let first = raw.split('\n').next().unwrap_or("");
    let first = first.strip_suffix('\r').unwrap_or(first);

    if first.is_empty() {
        return Err(format!(
            "{path} has no password on its first line"
        ));
    }

    Ok(first.to_string())
}

fn parse_u16(raw: &str) -> Result<u16, String> {
    raw.parse()
        .map_err(|_| format!("{raw} is not a number between 0 and {}", u16::MAX))
}
