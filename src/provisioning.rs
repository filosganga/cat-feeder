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
    // Through `decoded` rather than `field`, so that a port padded with spaces
    // is a port and not a field that is "too long for this firmware".
    let mqtt_port = decoded(body, "mqtt_port")?
        .ok_or(FormError::BadPort)?
        .trim()
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
        mqtt_user: optional_trimmed(body, "mqtt_user")?,
        mqtt_password: field(body, "mqtt_password")?.unwrap_or_default(),
        detent_ms: previous.map_or(DEFAULT_DETENT_MS, Record::detent_ms),
        portion_scale_pct: previous
            .map_or(crate::portions::SCALE_UNCHANGED, Record::portion_scale_pct),
    };

    Ok(record)
}

/// An optional field, decoded and trimmed before it is measured.
///
/// Absent and blank are the same thing here: every caller wants a default.
fn optional_trimmed<const N: usize>(
    body: &str,
    name: &'static str,
) -> Result<String<N>, FormError> {
    match decoded(body, name)? {
        Some(raw) => fit(trimmed(raw).as_str(), name),
        None => Ok(String::new()),
    }
}

fn required<const N: usize>(body: &str, name: &'static str) -> Result<String<N>, FormError> {
    // Decode, trim, *then* measure. The order is the point — see `decoded`.
    let raw = decoded(body, name)?.ok_or(FormError::Missing(name))?;
    let value = fit::<N>(trimmed(raw).as_str(), name)?;
    if value.is_empty() {
        return Err(FormError::Blank(name));
    }
    Ok(value)
}

/// Drops surrounding whitespace.
///
/// **Phone keyboards add trailing spaces**, by autocorrect, by autocomplete, or
/// by a thumb. An SSID is the field this ruins: `"fdlgrm "` is stored happily,
/// survives the reboot, and then fails forever as `NoAccessPointFound` — which
/// reads as "wrong password" or "out of range" and says nothing about the
/// space. Observed on the bench the first time this form was used for real.
///
/// **Passwords are deliberately not trimmed.** A password may legitimately end
/// in a space, and trimming one makes a correct credential unusable with no way
/// to express it. The risk is also much lower: `type=password` inputs do not
/// autocorrect or autocapitalise, which is what puts the space there in the
/// first place. The form sets those off explicitly on the other fields too.
///
/// Trimming can only shrink, so the result always fits.
fn trimmed<const N: usize>(value: String<N>) -> String<N> {
    let text = value.trim();
    if text.len() == value.len() {
        return value;
    }
    String::try_from(text).unwrap_or_default()
}

/// Finds one field and percent-decodes it.
///
/// Returns `None` when the field is absent, which the caller distinguishes from
/// a field that is present and empty.
pub fn field<const N: usize>(
    body: &str,
    name: &'static str,
) -> Result<Option<String<N>>, FormError> {
    match decoded(body, name)? {
        Some(value) => fit(&value, name).map(Some),
        None => Ok(None),
    }
}

/// Every field this firmware stores is far shorter than this, so decoding into
/// one size and narrowing afterwards costs a single stack buffer and buys the
/// ordering that [`required`] needs: **trim first, measure second**.
const DECODE_LEN: usize = 256;

/// Finds one field and percent-decodes it, without judging its length.
///
/// Split out from [`field`] so a value can be trimmed *before* it is measured.
/// Otherwise a 32-character SSID with the keyboard's trailing space is 33 bytes
/// and comes back as "too long" — refusing the one input the trimming exists to
/// rescue, with a message that names neither the field nor the space.
fn decoded(body: &str, name: &str) -> Result<Option<String<DECODE_LEN>>, FormError> {
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

/// Narrows a decoded value to the width the record has for it.
///
/// Refused rather than truncated: a shortened SSID would silently join the
/// wrong network, and a shortened password would fail to join at all.
fn fit<const N: usize>(value: &str, name: &'static str) -> Result<String<N>, FormError> {
    String::try_from(value).map_err(|_| FormError::TooLong(name))
}

/// Percent-decoding, plus the `+` for space that form encoding adds.
fn decode_value(raw: &str) -> Result<String<DECODE_LEN>, FormError> {
    // Decoded bytes first, because a multi-byte character arrives as several
    // escapes and only becomes valid UTF-8 once they are all decoded.
    let mut bytes: heapless::Vec<u8, DECODE_LEN> = heapless::Vec::new();
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

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// How much rendered HTML a page may take.
///
/// Sized against the worst case rather than the typical one, and
/// `the_widest_possible_form_still_fits` holds it to that: six fields at their
/// echo-back limit, every character one that escapes to six bytes, and the
/// longest error above them. `heapless` writes silently stop at the cap, so a
/// page that outgrew this would reach the phone truncated mid-tag with a
/// `Content-Length` agreeing with the truncation — a blank form and nothing on
/// the console.
pub const PAGE_LEN: usize = 8192;

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
/// new. The `Cache-Control: no-store` that `setup::send` sets keeps it out of
/// the browser's history.
///
/// That is a deliberate `code` span rather than a doc link: `setup` is private
/// and gated on the board target, so a link would resolve on neither a host
/// build nor `cargo doc`.
pub fn render_form(
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
    name: &'static str,
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

    // `autocapitalize`/`autocorrect`/`spellcheck` off on every field, and this
    // is the root of a real bug rather than polish: a phone keyboard offered
    // `fdlgrm` as a completion, inserted the trailing space that always follows
    // one, and the unit stored `"fdlgrm "` and then failed forever with
    // `NoAccessPointFound`. `provisioning::trimmed` is the belt; this is the
    // braces, and it also stops iOS capitalising the first letter of an SSID.
    let _ = write!(
        page,
        "<label for={name}>{label}</label>\
         <input id={name} name={name} type={kind} \
         autocapitalize=off autocorrect=off spellcheck=false \
         value=\"{}\"",
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
pub fn render_saved(page: &mut String<PAGE_LEN>, record: &Record) {
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
            Err(FormError::TooLong("wifi_ssid")),
            "a truncated SSID would silently join the wrong network"
        );
    }

    /// The six names the form and the parser must agree on.
    const FIELDS: [&str; 6] = [
        "wifi_ssid",
        "wifi_password",
        "mqtt_host",
        "mqtt_port",
        "mqtt_user",
        "mqtt_password",
    ];

    #[test]
    fn the_form_asks_for_exactly_what_the_parser_reads() {
        // Renaming an input on one side and not the other is invisible until a
        // phone posts a form with a field missing — which `record_from_form`
        // reports as `Missing`, blaming the person typing. Nothing else checks
        // this: the two lists are written out in different files.
        let mut page = String::<PAGE_LEN>::new();
        render_form(&mut page, "", None, None);

        for name in FIELDS {
            assert!(
                page.contains(&std::format!("name={name}")),
                "the form has no input named {name}"
            );
        }
        // Counted on `<input`, not on `name=`: the viewport `<meta>` carries a
        // `name=` too, and an assertion that drifts with the page furniture is
        // one somebody will eventually delete.
        assert_eq!(
            page.matches("<input").count(),
            FIELDS.len(),
            "the form has an input the parser does not read"
        );
    }

    #[test]
    fn what_the_form_shows_is_what_was_typed_into_it() {
        // The echo-back path end to end: a submitted body renders into the
        // page, and the values come back out of the HTML unchanged.
        let body = "wifi_ssid=My+Network&wifi_password=p%40ss&mqtt_host=10.0.0.1\
                    &mqtt_port=1883&mqtt_user=feeder&mqtt_password=dev";
        let mut page = String::<PAGE_LEN>::new();
        render_form(&mut page, body, Some(FormError::BadPort), None);

        assert!(page.contains(r#"value="My Network""#));
        assert!(page.contains(r#"value="p@ss""#));
        assert!(page.contains(r#"value="10.0.0.1""#));
        assert!(page.contains("The broker port must be a number"));
    }

    #[test]
    fn a_value_that_would_break_the_html_is_escaped_in_the_page() {
        // Not a hypothetical: an SSID may contain a quote, and unescaped it
        // ends the attribute early and silently truncates the field.
        let mut page = String::<PAGE_LEN>::new();
        render_form(&mut page, "wifi_ssid=a%22b%3Cc", None, None);

        assert!(page.contains(r#"value="a&quot;b&lt;c""#));
        // The raw quote must not survive anywhere in the rendered attribute.
        assert!(!page.contains(r#"value="a"b"#));
    }

    #[test]
    fn the_widest_possible_form_still_fits() {
        // `heapless` writes stop silently at the cap, so an overflowing page
        // reaches the phone truncated mid-tag with a `Content-Length` that
        // agrees with the truncation: a blank form, and nothing on the console
        // to say why. This is the only thing holding `PAGE_LEN` honest.
        //
        // Worst case: every field at its echo-back width, filled with the
        // character that costs the most escaped (`&` -> `&amp;`), and the
        // longest error message above them.
        let widest = "%26".repeat(PASSWORD_LEN);
        let mut body = std::string::String::new();
        for name in FIELDS {
            body.push_str(&std::format!("{name}={widest}&"));
        }

        let mut page = String::<PAGE_LEN>::new();
        render_form(
            &mut page,
            &body,
            Some(FormError::NotAnIp("mqtt_host")),
            None,
        );

        assert!(
            page.ends_with("</html>"),
            "the page was truncated: {} of {PAGE_LEN} bytes used",
            page.len()
        );
    }

    #[test]
    fn the_saved_page_names_the_network_it_is_leaving_for() {
        // The last thing anyone sees before the setup network disappears. If
        // it does not say where the unit went, a phone losing the access point
        // is indistinguishable from a crash.
        let mut page = String::<PAGE_LEN>::new();
        render_saved(&mut page, &sample());

        assert!(
            page.contains("fdlgrm"),
            "the SSID it is leaving for is missing"
        );
        assert!(page.contains("192.168.68.108"));
        assert!(page.contains("1883"));
        assert!(page.ends_with("</html>"));
        // The password reached this function inside the record and must not
        // reach the page: unlike the form, nobody needs to read it back.
        assert!(!page.contains("hunter2"));
        assert!(!page.contains("feeder-dev"));
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
    fn a_trailing_space_from_a_phone_keyboard_is_dropped() {
        // The bug this exists for, exactly as it happened: the SSID was stored
        // with a trailing space, the unit rebooted, and every attempt failed as
        // `NoAccessPointFound` — which reads as a wrong password and says
        // nothing whatever about a space.
        let body = "wifi_ssid=fdlgrm+&mqtt_host=+192.168.1.2+&mqtt_port=+1883+\
                    &mqtt_user=feeder+";
        let record = record_from_form(body, None).unwrap();

        assert_eq!(record.wifi_ssid.as_str(), "fdlgrm");
        assert_eq!(record.mqtt_host.as_str(), "192.168.1.2");
        assert_eq!(record.mqtt_port, 1883);
        assert_eq!(record.mqtt_user.as_str(), "feeder");
    }

    #[test]
    fn a_full_length_ssid_with_a_keyboard_space_still_fits() {
        // The boundary the first version of the trim got wrong: measuring
        // before trimming made a 32-character SSID plus the keyboard's space
        // 33 bytes, so it was refused as "too long" — the one input length
        // where the trimming still failed, and with a message naming neither
        // the field nor the space.
        let ssid = core::iter::repeat_n('x', SSID_LEN).collect::<std::string::String>();
        let body = std::format!("wifi_ssid={ssid}+&mqtt_host=10.0.0.1&mqtt_port=1883");

        assert_eq!(
            record_from_form(&body, None).unwrap().wifi_ssid.as_str(),
            ssid
        );
    }

    #[test]
    fn a_field_that_is_too_long_even_trimmed_names_itself() {
        // `TooLong` used to carry the literal "field", which `label` rendered
        // as "That field" — true, useless, and beside the wrong input.
        let long = core::iter::repeat_n('x', HOST_LEN + 1).collect::<std::string::String>();
        let body = std::format!("wifi_ssid=s&mqtt_host={long}&mqtt_port=1883");

        assert_eq!(
            record_from_form(&body, None),
            Err(FormError::TooLong("mqtt_host"))
        );
        assert!(std::format!("{}", FormError::TooLong("mqtt_host")).contains("broker address"));
    }

    #[test]
    fn a_padded_port_is_a_port_not_an_oversized_field() {
        // `mqtt_port` is read into eight bytes. Spaces around a four-digit
        // port fit, but the failure if they did not would be "too long for
        // this firmware" about a field the person typed four characters into.
        let record =
            record_from_form("wifi_ssid=s&mqtt_host=10.0.0.1&mqtt_port=++1883++", None).unwrap();
        assert_eq!(record.mqtt_port, 1883);
    }

    #[test]
    fn passwords_keep_every_character_they_were_given() {
        // The other half of the trade. A password may legitimately end in a
        // space, and trimming one makes a correct credential impossible to
        // enter — with the same silent failure and no way to work around it.
        let body = "wifi_ssid=net&wifi_password=+hunter2+\
                    &mqtt_host=10.0.0.1&mqtt_port=1883&mqtt_password=+s3cret+";
        let record = record_from_form(body, None).unwrap();

        assert_eq!(record.wifi_password.as_str(), " hunter2 ");
        assert_eq!(record.mqtt_password.as_str(), " s3cret ");
    }

    #[test]
    fn a_field_of_nothing_but_spaces_is_still_blank() {
        // Trimming must not turn "obviously empty" into "present and fine".
        assert_eq!(
            record_from_form("wifi_ssid=+++&mqtt_host=10.0.0.1&mqtt_port=1883", None),
            Err(FormError::Blank("wifi_ssid"))
        );
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
