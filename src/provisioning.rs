//! Runtime configuration: the record in flash, and parsing the setup form.
//!
//! Pure logic. No flash, no radio, no sockets — bytes in, bytes out — so all of
//! it is host-tested. The parts that touch hardware are thin wrappers around
//! what is here.
//!
//! ## How a unit gets configured
//!
//! ```text
//!   boot ── read the record from the nvs partition
//!            ├── valid   → station mode, connect, run normally
//!            └── missing → access point, serve the form, save, reboot
//!
//!   reset button held through power-on → erase the record  (lands in "missing")
//! ```
//!
//! There is exactly one way into setup, which is why the button erases rather
//! than signalling: "no valid record" is the only state the boot path has to
//! recognise.
//!
//! ## Why a checksum
//!
//! Erased flash reads back as `0xFF` everywhere, and a write interrupted by a
//! power cut leaves something halfway. Both must read as *no configuration*
//! rather than as garbage credentials, because a unit that believes a corrupt
//! record will sit trying to join a network that does not exist, with no way in
//! but the button. The magic catches blank flash and the CRC catches the rest.

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::str::FromStr as _;

use heapless::String;

/// 802.11 caps an SSID at 32 bytes.
pub const SSID_LEN: usize = 32;
/// A WPA2 passphrase is at most 63 characters; 64 leaves room for a PSK.
pub const PASSWORD_LEN: usize = 64;
/// Long enough for a hostname, not just the dotted quad the firmware currently
/// accepts.
pub const HOST_LEN: usize = 64;
pub const USER_LEN: usize = 32;

/// `FDR` for feeder, and a layout version. Bump it if the fields change: an
/// older record then fails to decode and the unit asks to be set up again,
/// which is the right outcome and better than reading fields at the wrong
/// offsets.
///
/// `FDR1` was credentials only. `FDR2` adds the two per-unit mechanical
/// figures, because three feeders that are not all the same model cannot share
/// one set of timings or one idea of how much a click dispenses.
const MAGIC: [u8; 4] = *b"FDR2";

/// Magic, checksum, six credential fields with their lengths, two mechanical
/// figures. Comfortably inside one 4 KB flash sector.
pub const MAX_RECORD_LEN: usize = 4
    + 4
    + (1 + SSID_LEN)
    + (1 + PASSWORD_LEN)
    + (1 + HOST_LEN)
    + 2
    + (1 + USER_LEN)
    + (1 + PASSWORD_LEN)
    + 2
    + 2;

/// A detent interval for a mechanism nobody has measured yet.
///
/// The figure from the two matching feeders. It is a starting point, not a
/// default worth relying on — the whole reason this is in the record is that
/// the third unit is a different brand.
pub const DEFAULT_DETENT_MS: u16 = 1_900;

/// Below this, the derived minimum click spacing would collide with the 30 ms
/// debounce in `switch.rs` and start rejecting real clicks.
pub const MIN_DETENT_MS: u16 = 200;

/// Everything a unit needs to reach the network and to run its own mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub wifi_ssid: String<SSID_LEN>,
    pub wifi_password: String<PASSWORD_LEN>,
    pub mqtt_host: String<HOST_LEN>,
    pub mqtt_port: u16,
    pub mqtt_user: String<USER_LEN>,
    pub mqtt_password: String<PASSWORD_LEN>,
    /// Milliseconds from one switch click to the next, under power.
    ///
    /// Read it through [`Record::detent_ms`], which clamps. The minimum click
    /// spacing and the jam timeout are both derived from this, so a zero here
    /// would mean a unit that jams the instant it starts.
    pub detent_ms: u16,
    /// How much this unit dispenses per click, as a percentage of a notional
    /// standard portion. 100 changes nothing.
    ///
    /// See [`crate::portions::clicks_for`].
    pub portion_scale_pct: u16,
}

/// Why a stored record could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// No record here. Erased flash reads as `0xFF`, which lands on this, and
    /// so does a unit that has never been set up.
    NotConfigured,
    /// The magic matched but the contents did not survive: an interrupted
    /// write, or a record written by different firmware.
    Corrupt,
    /// Ran off the end of the buffer partway through a field.
    Truncated,
    /// A field is longer than this firmware can hold.
    TooLong,
    /// A field is not UTF-8.
    NotUtf8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The destination buffer is smaller than [`MAX_RECORD_LEN`].
    NoRoom,
}

impl Record {
    /// Writes the record, returning how many bytes were used.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let mut at = 8; // magic and checksum are filled in last

        put_str(out, &mut at, &self.wifi_ssid)?;
        put_str(out, &mut at, &self.wifi_password)?;
        put_str(out, &mut at, &self.mqtt_host)?;
        put_bytes(out, &mut at, &self.mqtt_port.to_le_bytes())?;
        put_str(out, &mut at, &self.mqtt_user)?;
        put_str(out, &mut at, &self.mqtt_password)?;
        put_bytes(out, &mut at, &self.detent_ms.to_le_bytes())?;
        put_bytes(out, &mut at, &self.portion_scale_pct.to_le_bytes())?;

        let checksum = crc32(&out[8..at]).to_le_bytes();
        out[0..4].copy_from_slice(&MAGIC);
        out[4..8].copy_from_slice(&checksum);

        Ok(at)
    }

    /// Reads a record, or says why there isn't one.
    ///
    /// Takes the whole flash region rather than an exact slice, because the
    /// caller reading from flash does not know how long the record is until
    /// this has looked at it.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 8 || bytes[0..4] != MAGIC {
            return Err(DecodeError::NotConfigured);
        }

        let expected = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let body = &bytes[8..];
        let mut at = 0;

        let wifi_ssid = take_str(body, &mut at)?;
        let wifi_password = take_str(body, &mut at)?;
        let mqtt_host = take_str(body, &mut at)?;
        let mqtt_port = u16::from_le_bytes([take_u8(body, &mut at)?, take_u8(body, &mut at)?]);
        let mqtt_user = take_str(body, &mut at)?;
        let mqtt_password = take_str(body, &mut at)?;
        let detent_ms = u16::from_le_bytes([take_u8(body, &mut at)?, take_u8(body, &mut at)?]);
        let portion_scale_pct =
            u16::from_le_bytes([take_u8(body, &mut at)?, take_u8(body, &mut at)?]);

        // Checked only once the length is known, so trailing flash beyond the
        // record cannot change the answer.
        if crc32(&body[..at]) != expected {
            return Err(DecodeError::Corrupt);
        }

        Ok(Self {
            wifi_ssid,
            wifi_password,
            mqtt_host,
            mqtt_port,
            mqtt_user,
            mqtt_password,
            detent_ms,
            portion_scale_pct,
        })
    }

    /// Whether this record is worth trying to connect with.
    ///
    /// An empty SSID or host cannot work, and a unit that tries anyway looks
    /// broken rather than unconfigured. The form rejects these too; this is the
    /// backstop for a record written by an older or buggier version.
    pub fn is_usable(&self) -> bool {
        !self.wifi_ssid.is_empty() && !self.mqtt_host.is_empty() && self.mqtt_port != 0
    }

    /// The detent interval, clamped to something a mechanism could actually do.
    ///
    /// Clamped rather than validated in [`Record::is_usable`] on purpose: a
    /// nonsensical mechanical figure must not make a unit *unconfigurable*. It
    /// can still reach the broker, still answer Home Assistant, and still be
    /// re-provisioned — all of which a rejected record would prevent, leaving
    /// the button as the only way back.
    pub fn detent_ms(&self) -> u16 {
        if self.detent_ms < MIN_DETENT_MS {
            DEFAULT_DETENT_MS
        } else {
            self.detent_ms
        }
    }

    /// The portion scale, with zero read as "unchanged".
    ///
    /// Zero would otherwise round every meal to the never-zero floor of one
    /// click, which looks like working and is not.
    pub fn portion_scale_pct(&self) -> u16 {
        if self.portion_scale_pct == 0 {
            crate::portions::SCALE_UNCHANGED
        } else {
            self.portion_scale_pct
        }
    }
}

fn put_bytes(out: &mut [u8], at: &mut usize, bytes: &[u8]) -> Result<(), EncodeError> {
    let end = *at + bytes.len();
    if end > out.len() {
        return Err(EncodeError::NoRoom);
    }
    out[*at..end].copy_from_slice(bytes);
    *at = end;
    Ok(())
}

fn put_str<const N: usize>(
    out: &mut [u8],
    at: &mut usize,
    text: &String<N>,
) -> Result<(), EncodeError> {
    // Lengths are a single byte, which every field size here stays under.
    put_bytes(out, at, &[text.len() as u8])?;
    put_bytes(out, at, text.as_bytes())
}

fn take_u8(body: &[u8], at: &mut usize) -> Result<u8, DecodeError> {
    let byte = *body.get(*at).ok_or(DecodeError::Truncated)?;
    *at += 1;
    Ok(byte)
}

fn take_str<const N: usize>(body: &[u8], at: &mut usize) -> Result<String<N>, DecodeError> {
    let len = take_u8(body, at)? as usize;
    if len > N {
        return Err(DecodeError::TooLong);
    }

    let end = *at + len;
    let raw = body.get(*at..end).ok_or(DecodeError::Truncated)?;
    *at = end;

    let text = core::str::from_utf8(raw).map_err(|_| DecodeError::NotUtf8)?;
    String::try_from(text).map_err(|_| DecodeError::TooLong)
}

/// CRC-32/ISO-HDLC, computed a bit at a time.
///
/// No lookup table: this runs twice per boot at most, and 1 KB of table is
/// worth more than the microseconds it would save.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// The setup network
// ---------------------------------------------------------------------------

/// `cat-feeder-` plus the six-character device id.
pub const AP_SSID_LEN: usize = 17;

/// Twelve symbols in three groups of four, `K7M2-QH9X-4TRN`.
pub const AP_PASSWORD_LEN: usize = 14;

/// Crockford's base32: no `I`, `L`, `O` or `U`, so nothing on a sticker can be
/// misread as something else, and no word can accidentally appear.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The name of the setup network, which is also how you tell three feeders
/// apart while holding a phone in front of them.
pub fn ap_ssid(device_id: &str) -> String<AP_SSID_LEN> {
    let mut ssid = String::new();
    let _ = ssid.push_str("cat-feeder-");
    let _ = ssid.push_str(device_id);
    ssid
}

/// The setup network's password: unique per unit, and not derivable from
/// anything the unit broadcasts.
///
/// Deriving it from the MAC alone would not be a secret at all. The MAC is in
/// the SSID, it is the BSSID in every beacon frame, and this function is public
/// — so anyone in range could compute it. That matters because WPA2-PSK gives
/// no protection against someone who knows the passphrase: they can capture the
/// handshake and read the session, which is the one where the home Wi-Fi
/// password gets typed into the form.
///
/// The secret comes from `cfg.toml` at build time, like the rest. It is not the
/// home Wi-Fi password and it never leaves the unit, so it is the only
/// build-time value that survives this feature.
///
/// Reproduced by `dev/ap-password.sh` so stickers can be printed before a unit
/// is first powered on. Both sides must agree, which is why this is plain
/// SHA-256 over `<secret>:<device id>` and nothing more inventive.
pub fn ap_password(secret: &str, device_id: &str) -> String<AP_PASSWORD_LEN> {
    let mut input: heapless::Vec<u8, 128> = heapless::Vec::new();
    let _ = input.extend_from_slice(secret.as_bytes());
    let _ = input.push(b':');
    let _ = input.extend_from_slice(device_id.as_bytes());

    let digest = crate::sha256::sha256(&input);

    // Sixty bits off the front of the digest, five at a time. Far more than
    // enough for a network nobody can reach without being in the room.
    let bits = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);

    let mut password = String::new();
    for symbol in 0u32..12 {
        if symbol > 0 && symbol.is_multiple_of(4) {
            let _ = password.push('-');
        }
        let index = (bits >> (59 - 5 * symbol)) & 0x1F;
        let _ = password.push(ALPHABET[index as usize] as char);
    }
    password
}

// ---------------------------------------------------------------------------
// The setup form
// ---------------------------------------------------------------------------

/// Why a submitted form could not be turned into a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    /// A required field was absent.
    Missing(&'static str),
    /// A field was longer than this firmware can store.
    TooLong(&'static str),
    /// A percent-escape was malformed, or the result was not UTF-8.
    BadEncoding(&'static str),
    /// The port was absent, not a number, or zero.
    BadPort,
    /// A field that cannot be blank was blank.
    Blank(&'static str),
    /// The broker address was not a literal IPv4 address.
    ///
    /// **This firmware has no resolver.** `mqtt.rs` parses `mqtt_host` with
    /// `Ipv4Addr::from_str` and gives up if that fails, so a hostname typed
    /// here would be stored, survive a reboot, and leave the unit retrying a
    /// connection it can never make — with the console the only place saying
    /// why. Refusing it at the form is the one moment somebody is standing
    /// there able to fix it.
    NotAnIp(&'static str),
}

/// What to call a form field when talking to a person.
///
/// The variants above carry the *form* field name, because that is what the
/// HTML needs in order to put the message beside the right input. A person
/// reading the page wants the label instead.
fn label(field: &str) -> &'static str {
    match field {
        "wifi_ssid" => "The Wi-Fi network name",
        "wifi_password" => "The Wi-Fi password",
        "mqtt_host" => "The broker address",
        "mqtt_port" => "The broker port",
        "mqtt_user" => "The broker username",
        "mqtt_password" => "The broker password",
        // `decode_value` reports "field" and "percent"/"utf-8" rather than a
        // name, because it does not know which field it is decoding.
        _ => "That field",
    }
}

impl core::fmt::Display for FormError {
    /// The sentence shown on the form. Written for whoever is holding the
    /// phone, so it says what to do rather than what went wrong internally.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing(field) => write!(f, "{} is missing.", label(field)),
            Self::TooLong(field) => write!(f, "{} is too long for this firmware.", label(field)),
            Self::BadEncoding(_) => write!(f, "That form could not be decoded. Try again."),
            Self::BadPort => write!(f, "The broker port must be a number from 1 to 65535."),
            Self::Blank(field) => write!(f, "{} cannot be empty.", label(field)),
            Self::NotAnIp(field) => write!(
                f,
                "{} must be an IP address such as 192.168.1.10. This firmware has no DNS, so a name will not work.",
                label(field)
            ),
        }
    }
}

/// A string with the four characters that would break an HTML attribute
/// replaced by their entities.
///
/// Wrapping rather than allocating, because the page is written straight into
/// the response buffer and there is nowhere to put a second copy.
///
/// This is about **correctness before safety**. An SSID may legitimately
/// contain `&` or `"`, and an unescaped `"` ends the `value="..."` attribute
/// early: the field silently loses the rest of its contents, and the person
/// re-typing it has no idea why. That the same escaping also stops an SSID
/// closing a tag is a second reason, not the first one.
#[derive(Debug, Clone, Copy)]
pub struct Escaped<'a>(pub &'a str);

impl core::fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for ch in self.0.chars() {
            match ch {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                _ => f.write_char(ch)?,
            }
        }
        Ok(())
    }
}

/// Builds a record from an `application/x-www-form-urlencoded` body.
///
/// The Wi-Fi password may legitimately be empty, for an open network. Nothing
/// else may: the broker credentials are optional in MQTT but the dev stack
/// requires them, and an empty SSID or host cannot work at all.
///
/// **`previous` is what the unit already had, and it carries the mechanical
/// figures across.** The setup form asks for a network and a broker, because
/// that is what someone with a phone can answer; the detent interval and the
/// portion scale are bench measurements and have no business on it. Without
/// this argument, re-provisioning a working feeder after a Wi-Fi password
/// change would silently reset its calibration to defaults, and the only
/// symptom would be that one feeder quietly dispenses the wrong amount.
///
/// It is a parameter rather than a merge the caller remembers to do, because
/// forgetting it is invisible until someone weighs the food.
pub fn record_from_form(body: &str, previous: Option<&Record>) -> Result<Record, FormError> {
    let mqtt_port = field::<8>(body, "mqtt_port")?
        .ok_or(FormError::BadPort)?
        .parse::<u16>()
        .map_err(|_| FormError::BadPort)?;
    if mqtt_port == 0 {
        return Err(FormError::BadPort);
    }

    let mqtt_host = required(body, "mqtt_host")?;
    // No resolver on this device, so a name is not a thing that can be tried
    // later — see `FormError::NotAnIp`.
    if Ipv4Addr::from_str(&mqtt_host).is_err() {
        return Err(FormError::NotAnIp("mqtt_host"));
    }

    let record = Record {
        wifi_ssid: required(body, "wifi_ssid")?,
        wifi_password: field(body, "wifi_password")?.unwrap_or_default(),
        mqtt_host,
        mqtt_port,
        mqtt_user: field(body, "mqtt_user")?.unwrap_or_default(),
        mqtt_password: field(body, "mqtt_password")?.unwrap_or_default(),
        detent_ms: previous.map_or(DEFAULT_DETENT_MS, Record::detent_ms),
        portion_scale_pct: previous
            .map_or(crate::portions::SCALE_UNCHANGED, Record::portion_scale_pct),
    };

    Ok(record)
}

fn required<const N: usize>(body: &str, name: &'static str) -> Result<String<N>, FormError> {
    let value = field::<N>(body, name)?.ok_or(FormError::Missing(name))?;
    if value.is_empty() {
        return Err(FormError::Blank(name));
    }
    Ok(value)
}

/// Finds one field and percent-decodes it.
///
/// Returns `None` when the field is absent, which the caller distinguishes from
/// a field that is present and empty.
pub fn field<const N: usize>(body: &str, name: &str) -> Result<Option<String<N>>, FormError> {
    for pair in body.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            // A bare key with no `=`. Browsers do not send these; skip it
            // rather than reject the whole form.
            continue;
        };
        if key == name {
            return decode_value(value).map(Some);
        }
    }
    Ok(None)
}

/// Percent-decoding, plus the `+` for space that form encoding adds.
fn decode_value<const N: usize>(raw: &str) -> Result<String<N>, FormError> {
    // Decoded bytes first, because a multi-byte character arrives as several
    // escapes and only becomes valid UTF-8 once they are all decoded.
    let mut bytes: heapless::Vec<u8, N> = heapless::Vec::new();
    let mut chars = raw.as_bytes().iter().copied();

    while let Some(byte) = chars.next() {
        let decoded = match byte {
            b'+' => b' ',
            b'%' => {
                let hi = chars.next().ok_or(FormError::BadEncoding("percent"))?;
                let lo = chars.next().ok_or(FormError::BadEncoding("percent"))?;
                hex(hi)? << 4 | hex(lo)?
            }
            other => other,
        };
        bytes
            .push(decoded)
            .map_err(|_| FormError::TooLong("field"))?;
    }

    let text = core::str::from_utf8(&bytes).map_err(|_| FormError::BadEncoding("utf-8"))?;
    String::try_from(text).map_err(|_| FormError::TooLong("field"))
}

fn hex(byte: u8) -> Result<u8, FormError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(FormError::BadEncoding("percent")),
    }
}

// ---------------------------------------------------------------------------
// Just enough HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    /// Anything else. Answered with 405 rather than parsed.
    Other,
}

/// The head of a request: everything up to the blank line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub method: Method,
    /// Path with any query string removed.
    pub path: String<64>,
    pub content_length: usize,
    /// Where the body starts in the buffer the head was parsed from.
    pub body_at: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// The request line was not `METHOD PATH VERSION`.
    Malformed,
    /// The path, or a header this cares about, was longer than expected.
    TooLong,
}

/// Parses a request head, or reports that more bytes are needed.
///
/// Sans-IO on purpose: the socket loop keeps reading until this stops
/// returning `Ok(None)`, and there is nothing to fake in a test.
pub fn parse_head(buf: &[u8]) -> Result<Option<Head>, HttpError> {
    let Some(end) = find(buf, b"\r\n\r\n") else {
        return Ok(None);
    };
    let body_at = end + 4;

    let head = core::str::from_utf8(&buf[..end]).map_err(|_| HttpError::Malformed)?;
    let mut lines = head.split("\r\n");

    let request_line = lines.next().ok_or(HttpError::Malformed)?;
    let mut parts = request_line.split(' ');
    let method = match parts.next().ok_or(HttpError::Malformed)? {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => Method::Other,
    };
    let target = parts.next().ok_or(HttpError::Malformed)?;
    // A captive-portal probe or a browser may append a query string; the paths
    // this serves never use one.
    let path = target.split('?').next().unwrap_or(target);

    let mut content_length = 0;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| HttpError::Malformed)?;
        }
    }

    Ok(Some(Head {
        method,
        path: String::try_from(path).map_err(|_| HttpError::TooLong)?,
        content_length,
        body_at,
    }))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        Record {
            wifi_ssid: String::try_from("fdlgrm").unwrap(),
            wifi_password: String::try_from("hunter2").unwrap(),
            mqtt_host: String::try_from("192.168.68.108").unwrap(),
            mqtt_port: 1883,
            mqtt_user: String::try_from("feeder").unwrap(),
            mqtt_password: String::try_from("feeder-dev").unwrap(),
            detent_ms: DEFAULT_DETENT_MS,
            portion_scale_pct: crate::portions::SCALE_UNCHANGED,
        }
    }

    #[test]
    fn the_form_keeps_the_calibration_it_was_not_asked_about() {
        // The setup form asks for a network and a broker, because that is what
        // someone holding a phone can answer. Re-provisioning after a Wi-Fi
        // password change must not quietly reset a bench measurement — the only
        // symptom would be one feeder dispensing the wrong amount of food.
        let calibrated = Record {
            detent_ms: 850,
            portion_scale_pct: 133,
            ..sample()
        };

        let body = "wifi_ssid=new&wifi_password=pw&mqtt_host=10.0.0.1&mqtt_port=1883";
        let updated = record_from_form(body, Some(&calibrated)).unwrap();

        assert_eq!(updated.wifi_ssid.as_str(), "new");
        assert_eq!(updated.detent_ms, 850);
        assert_eq!(updated.portion_scale_pct, 133);
    }

    #[test]
    fn a_first_time_form_gets_the_defaults() {
        let body = "wifi_ssid=new&mqtt_host=10.0.0.1&mqtt_port=1883";
        let fresh = record_from_form(body, None).unwrap();

        assert_eq!(fresh.detent_ms, DEFAULT_DETENT_MS);
        assert_eq!(fresh.portion_scale_pct, crate::portions::SCALE_UNCHANGED);
    }

    #[test]
    fn nonsense_mechanical_figures_do_not_make_a_unit_unconfigurable() {
        // Clamped on read rather than rejected: a bad calibration must still
        // leave a feeder able to reach the broker and be re-provisioned. A
        // rejected record would leave the button as the only way back.
        let broken = Record {
            detent_ms: 0,
            portion_scale_pct: 0,
            ..sample()
        };

        assert!(broken.is_usable());
        assert_eq!(broken.detent_ms(), DEFAULT_DETENT_MS);
        assert_eq!(broken.portion_scale_pct(), crate::portions::SCALE_UNCHANGED);
    }

    #[test]
    fn an_older_record_is_refused_rather_than_misread() {
        // An FDR1 record has no mechanical fields. Reading one as FDR2 would
        // take the CRC bytes as a detent interval, so the magic has to reject
        // it outright and send the unit back to setup.
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = sample().encode(&mut buffer).unwrap();
        buffer[3] = b'1';

        assert_eq!(
            Record::decode(&buffer[..len]),
            Err(DecodeError::NotConfigured)
        );
    }

    // ---- the record ----

    #[test]
    fn a_record_survives_a_round_trip() {
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = sample().encode(&mut buffer).unwrap();

        assert_eq!(Record::decode(&buffer[..len]).unwrap(), sample());
    }

    #[test]
    fn decoding_ignores_whatever_follows_the_record() {
        // Flash is read a sector at a time, so the caller hands over far more
        // bytes than the record occupies.
        let mut buffer = [0xFFu8; 512];
        sample().encode(&mut buffer).unwrap();

        assert_eq!(Record::decode(&buffer).unwrap(), sample());
    }

    #[test]
    fn erased_flash_reads_as_unconfigured() {
        // The state every unit ships in, and the one the reset button returns
        // it to. It must not look like an error.
        assert_eq!(
            Record::decode(&[0xFFu8; 512]),
            Err(DecodeError::NotConfigured)
        );
        assert_eq!(
            Record::decode(&[0x00u8; 512]),
            Err(DecodeError::NotConfigured)
        );
        assert_eq!(Record::decode(&[]), Err(DecodeError::NotConfigured));
    }

    #[test]
    fn no_single_flipped_bit_yields_a_usable_record() {
        // Two mechanisms share the work and which one fires depends on where
        // the flip lands: a damaged length byte runs the fields out of step and
        // is caught as it is read, a damaged content byte reaches the checksum.
        // What matters is that neither ever produces credentials.
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = sample().encode(&mut buffer).unwrap();

        for byte in 0..len {
            for bit in 0..8 {
                let mut damaged = buffer;
                damaged[byte] ^= 1 << bit;
                assert!(
                    Record::decode(&damaged[..len]).is_err(),
                    "bit {bit} of byte {byte} flipped and the record still decoded"
                );
            }
        }
    }

    #[test]
    fn the_checksum_is_what_catches_damaged_content() {
        // Byte 9 is inside the SSID: the lengths still line up, so this gets
        // all the way to the checksum. Without it, the unit would come up
        // trying to join a network whose name is one letter wrong.
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = sample().encode(&mut buffer).unwrap();
        buffer[9] ^= 0x01;

        assert_eq!(
            Record::decode(&buffer[..len]),
            Err(DecodeError::Corrupt),
            "the checksum should be the thing that notices"
        );
    }

    #[test]
    fn a_half_written_record_is_not_credentials() {
        // A power cut during the write. The magic lands first, so only the
        // checksum stands between this and a unit chasing a network that was
        // never configured.
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = sample().encode(&mut buffer).unwrap();

        for cut in 9..len {
            let mut partial = buffer;
            partial[cut..].fill(0xFF);
            assert!(
                Record::decode(&partial).is_err(),
                "a write cut at byte {cut} decoded as a valid record"
            );
        }
    }

    #[test]
    fn the_longest_fields_still_fit() {
        let long = |n: usize| core::iter::repeat_n('x', n).collect::<std::string::String>();
        let record = Record {
            wifi_ssid: String::try_from(long(SSID_LEN).as_str()).unwrap(),
            wifi_password: String::try_from(long(PASSWORD_LEN).as_str()).unwrap(),
            mqtt_host: String::try_from(long(HOST_LEN).as_str()).unwrap(),
            mqtt_port: u16::MAX,
            mqtt_user: String::try_from(long(USER_LEN).as_str()).unwrap(),
            mqtt_password: String::try_from(long(PASSWORD_LEN).as_str()).unwrap(),
            detent_ms: u16::MAX,
            portion_scale_pct: u16::MAX,
        };

        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = record.encode(&mut buffer).unwrap();
        assert_eq!(len, MAX_RECORD_LEN);
        assert_eq!(Record::decode(&buffer[..len]).unwrap(), record);
    }

    #[test]
    fn an_empty_field_round_trips() {
        // An open Wi-Fi network, or a broker with no credentials.
        let mut record = sample();
        record.wifi_password = String::new();
        record.mqtt_user = String::new();
        record.mqtt_password = String::new();

        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = record.encode(&mut buffer).unwrap();
        assert_eq!(Record::decode(&buffer[..len]).unwrap(), record);
    }

    #[test]
    fn a_record_that_cannot_work_is_not_usable() {
        assert!(sample().is_usable());

        let mut blank_ssid = sample();
        blank_ssid.wifi_ssid = String::new();
        assert!(!blank_ssid.is_usable());

        let mut no_port = sample();
        no_port.mqtt_port = 0;
        assert!(!no_port.is_usable());
    }

    // ---- the setup network ----

    #[test]
    fn the_ssid_names_the_unit() {
        assert_eq!(ap_ssid("db0260").as_str(), "cat-feeder-db0260");
        // Exactly fills the buffer, so nothing is silently dropped.
        assert_eq!(ap_ssid("db0260").len(), AP_SSID_LEN);
    }

    #[test]
    fn the_password_is_shaped_for_reading_off_a_sticker() {
        let password = ap_password("s3cr3t", "db0260");

        assert_eq!(password.len(), AP_PASSWORD_LEN);
        assert_eq!(password.chars().filter(|c| *c == '-').count(), 2);
        assert!(
            password
                .chars()
                .all(|c| c == '-' || ALPHABET.contains(&(c as u8))),
            "{password} strayed outside the alphabet"
        );
        // WPA2 will not accept anything shorter than eight characters.
        assert!(password.len() >= 8);
    }

    #[test]
    fn the_password_is_stable() {
        // The unit and dev/ap-password.sh derive this independently, so a
        // change here silently invalidates every sticker already printed.
        //
        // These values come from a separate Python implementation of the same
        // rule, not from this code, so the test pins the definition rather than
        // whatever the implementation happens to do.
        assert_eq!(ap_password("s3cr3t", "db0260").as_str(), "55KA-G8H6-9NMQ");
        assert_eq!(ap_password("s3cr3t", "db0261").as_str(), "BPF0-16PA-MXSN");
        assert_eq!(ap_password("one", "db0260").as_str(), "5K3Z-ZCX0-H9KP");
    }

    #[test]
    fn each_unit_gets_a_different_password() {
        let secret = "s3cr3t";
        let a = ap_password(secret, "db0260");
        let b = ap_password(secret, "db0261");
        let c = ap_password(secret, "aabbcc");

        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn a_different_secret_gives_a_different_password() {
        // The whole point: the device id is public, so it must not be the only
        // input. Changing the secret has to change the answer.
        assert_ne!(ap_password("one", "db0260"), ap_password("two", "db0260"));
    }

    // ---- the form ----

    #[test]
    fn a_submitted_form_becomes_a_record() {
        let body = "wifi_ssid=fdlgrm&wifi_password=hunter2\
                    &mqtt_host=192.168.68.108&mqtt_port=1883\
                    &mqtt_user=feeder&mqtt_password=feeder-dev";

        assert_eq!(record_from_form(body, None).unwrap(), sample());
    }

    #[test]
    fn form_encoding_is_decoded() {
        // A password of `p@ss word+1%` as a browser would send it.
        let body = "wifi_ssid=My+Network&wifi_password=p%40ss+word%2B1%25\
                    &mqtt_host=10.0.0.1&mqtt_port=1883";
        let record = record_from_form(body, None).unwrap();

        assert_eq!(record.wifi_ssid.as_str(), "My Network");
        assert_eq!(record.wifi_password.as_str(), "p@ss word+1%");
    }

    #[test]
    fn a_multibyte_character_survives_decoding() {
        // Decoding has to assemble the bytes before checking UTF-8, or an
        // accented SSID comes back as an encoding error.
        let body = "wifi_ssid=Caff%C3%A8&mqtt_host=10.0.0.1&mqtt_port=1883";
        assert_eq!(
            record_from_form(body, None).unwrap().wifi_ssid.as_str(),
            "Caffè"
        );
    }

    #[test]
    fn an_open_network_needs_no_password() {
        let body = "wifi_ssid=Open&wifi_password=&mqtt_host=10.0.0.1&mqtt_port=1883";
        let record = record_from_form(body, None).unwrap();

        assert!(record.wifi_password.is_empty());
        assert!(record.is_usable());
    }

    #[test]
    fn the_fields_that_cannot_be_blank_are_rejected() {
        assert_eq!(
            record_from_form("wifi_ssid=&mqtt_host=10.0.0.1&mqtt_port=1883", None),
            Err(FormError::Blank("wifi_ssid"))
        );
        assert_eq!(
            record_from_form("wifi_ssid=s&mqtt_host=&mqtt_port=1883", None),
            Err(FormError::Blank("mqtt_host"))
        );
        assert_eq!(
            record_from_form("mqtt_host=10.0.0.1&mqtt_port=1883", None),
            Err(FormError::Missing("wifi_ssid"))
        );
    }

    #[test]
    fn a_bad_port_is_rejected_rather_than_defaulted() {
        // Quietly falling back to 1883 would hide a typo until the unit failed
        // to connect, with nothing on the form to show for it.
        for body in [
            "wifi_ssid=s&mqtt_host=10.0.0.1",
            "wifi_ssid=s&mqtt_host=10.0.0.1&mqtt_port=",
            "wifi_ssid=s&mqtt_host=10.0.0.1&mqtt_port=0",
            "wifi_ssid=s&mqtt_host=10.0.0.1&mqtt_port=99999",
            "wifi_ssid=s&mqtt_host=10.0.0.1&mqtt_port=1883x",
        ] {
            assert_eq!(
                record_from_form(body, None),
                Err(FormError::BadPort),
                "{body}"
            );
        }
    }

    #[test]
    fn an_oversized_field_is_refused_not_truncated() {
        let long = core::iter::repeat_n('x', SSID_LEN + 1).collect::<std::string::String>();
        let body = std::format!("wifi_ssid={long}&mqtt_host=10.0.0.1&mqtt_port=1883");

        assert_eq!(
            record_from_form(&body, None),
            Err(FormError::TooLong("field")),
            "a truncated SSID would silently join the wrong network"
        );
    }

    #[test]
    fn a_hostname_is_refused_because_there_is_no_resolver() {
        // The failure this prevents is the quiet one: a name would be stored,
        // survive the reboot, and leave the unit retrying a connection it can
        // never make, with the console the only place that says why.
        for host in [
            "broker.local",
            "homeassistant",
            "mqtt.example.com",
            "192.168.1",
            "192.168.1.256",
            "1.2.3.4.5",
            "::1",
            " 192.168.1.10",
        ] {
            let body = std::format!("wifi_ssid=s&mqtt_host={host}&mqtt_port=1883");
            assert_eq!(
                record_from_form(&body, None),
                Err(FormError::NotAnIp("mqtt_host")),
                "{host}"
            );
        }
    }

    #[test]
    fn a_dotted_quad_is_accepted() {
        for host in ["192.168.1.10", "10.0.0.1", "127.0.0.1", "0.0.0.0"] {
            let body = std::format!("wifi_ssid=s&mqtt_host={host}&mqtt_port=1883");
            assert_eq!(
                record_from_form(&body, None).unwrap().mqtt_host.as_str(),
                host
            );
        }
    }

    #[test]
    fn the_port_is_judged_before_the_host() {
        // Both are wrong here. Reporting the port first is what the form shows,
        // and swapping the order would change the message under someone's
        // fingers for no reason.
        assert_eq!(
            record_from_form("wifi_ssid=s&mqtt_host=nope&mqtt_port=0", None),
            Err(FormError::BadPort)
        );
    }

    #[test]
    fn every_form_error_says_something_a_person_can_act_on() {
        // Rendered into the page, so it must name the field in words rather
        // than echo the HTML input name at somebody.
        let cases = [
            (FormError::Missing("wifi_ssid"), "Wi-Fi network name"),
            (FormError::Blank("mqtt_host"), "broker address"),
            (FormError::TooLong("wifi_password"), "Wi-Fi password"),
            (FormError::NotAnIp("mqtt_host"), "192.168.1.10"),
            (FormError::BadPort, "65535"),
            (FormError::BadEncoding("utf-8"), "could not be decoded"),
        ];
        for (error, expected) in cases {
            let rendered = std::format!("{error}");
            assert!(
                rendered.contains(expected),
                "{error:?} rendered as {rendered:?}, wanted {expected:?}"
            );
            assert!(rendered.ends_with('.'), "{rendered:?} is not a sentence");
            // The raw form field name must not leak into the page.
            assert!(!rendered.contains('_'), "{rendered:?} leaks a field name");
        }
    }

    #[test]
    fn a_quote_in_an_ssid_cannot_end_the_attribute_early() {
        // The bug this prevents is not exotic: the field silently loses
        // everything after the quote and the person re-typing it never learns
        // why.
        let rendered = std::format!("{}", Escaped("Bob\"s <net> & co"));
        assert_eq!(rendered, "Bob&quot;s &lt;net&gt; &amp; co");
        assert!(!rendered.contains('"'));
        assert!(!rendered.contains('<'));
    }

    #[test]
    fn escaping_leaves_an_ordinary_ssid_alone() {
        // Including the characters a Wi-Fi name really does contain. Escaping
        // an apostrophe or a space would show mojibake on the form.
        for plain in ["My Network", "Caffe\u{300}", "it's-5GHz_2", ""] {
            assert_eq!(std::format!("{}", Escaped(plain)), plain);
        }
    }

    #[test]
    fn a_broken_percent_escape_is_an_error() {
        for body in [
            "wifi_ssid=a%&mqtt_host=10.0.0.1&mqtt_port=1883",
            "wifi_ssid=a%4&mqtt_host=10.0.0.1&mqtt_port=1883",
            "wifi_ssid=a%zz&mqtt_host=10.0.0.1&mqtt_port=1883",
        ] {
            assert!(matches!(
                record_from_form(body, None),
                Err(FormError::BadEncoding(_))
            ));
        }
    }

    // ---- HTTP ----

    #[test]
    fn an_incomplete_head_asks_for_more() {
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n"), Ok(None));
        assert_eq!(parse_head(b""), Ok(None));
    }

    #[test]
    fn a_get_is_parsed() {
        let head = parse_head(b"GET / HTTP/1.1\r\nHost: 192.168.4.1\r\n\r\n")
            .unwrap()
            .unwrap();

        assert_eq!(head.method, Method::Get);
        assert_eq!(head.path.as_str(), "/");
        assert_eq!(head.content_length, 0);
    }

    #[test]
    fn a_post_reports_its_body() {
        let raw = b"POST /save HTTP/1.1\r\nHost: x\r\nContent-Length: 9\r\n\r\nssid=home";
        let head = parse_head(raw).unwrap().unwrap();

        assert_eq!(head.method, Method::Post);
        assert_eq!(head.path.as_str(), "/save");
        assert_eq!(head.content_length, 9);
        assert_eq!(&raw[head.body_at..], b"ssid=home");
    }

    #[test]
    fn header_names_are_case_insensitive() {
        // Not every client spells it Content-Length.
        let head = parse_head(b"POST /save HTTP/1.1\r\ncontent-length: 4\r\n\r\nabcd")
            .unwrap()
            .unwrap();
        assert_eq!(head.content_length, 4);
    }

    #[test]
    fn a_query_string_is_dropped_from_the_path() {
        let head = parse_head(b"GET /generate_204?x=1 HTTP/1.1\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(head.path.as_str(), "/generate_204");
    }

    #[test]
    fn an_unknown_method_is_recognised_as_such() {
        let head = parse_head(b"DELETE / HTTP/1.1\r\n\r\n").unwrap().unwrap();
        assert_eq!(head.method, Method::Other);
    }
}
