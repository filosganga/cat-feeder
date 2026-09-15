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
//!   reset button held → erase the record, reboot  (lands in "missing")
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

use heapless::String;

/// 802.11 caps an SSID at 32 bytes.
pub const SSID_LEN: usize = 32;
/// A WPA2 passphrase is at most 63 characters; 64 leaves room for a PSK.
pub const PASSWORD_LEN: usize = 64;
/// Long enough for a hostname, not just the dotted quad the firmware currently
/// accepts.
pub const HOST_LEN: usize = 64;
pub const USER_LEN: usize = 32;

/// `FDR` for feeder, `1` for the layout below. Bump it if the fields change:
/// an older record then fails to decode and the unit asks to be set up again,
/// which is the right outcome and better than reading fields at the wrong
/// offsets.
const MAGIC: [u8; 4] = *b"FDR1";

/// Magic, checksum, six fields with their lengths. Comfortably inside one
/// 4 KB flash sector.
pub const MAX_RECORD_LEN: usize = 4
    + 4
    + (1 + SSID_LEN)
    + (1 + PASSWORD_LEN)
    + (1 + HOST_LEN)
    + 2
    + (1 + USER_LEN)
    + (1 + PASSWORD_LEN);

/// Everything a unit needs to reach the network and the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub wifi_ssid: String<SSID_LEN>,
    pub wifi_password: String<PASSWORD_LEN>,
    pub mqtt_host: String<HOST_LEN>,
    pub mqtt_port: u16,
    pub mqtt_user: String<USER_LEN>,
    pub mqtt_password: String<PASSWORD_LEN>,
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
}

/// Builds a record from an `application/x-www-form-urlencoded` body.
///
/// The Wi-Fi password may legitimately be empty, for an open network. Nothing
/// else may: the broker credentials are optional in MQTT but the dev stack
/// requires them, and an empty SSID or host cannot work at all.
pub fn record_from_form(body: &str) -> Result<Record, FormError> {
    let mqtt_port = field::<8>(body, "mqtt_port")?
        .ok_or(FormError::BadPort)?
        .parse::<u16>()
        .map_err(|_| FormError::BadPort)?;
    if mqtt_port == 0 {
        return Err(FormError::BadPort);
    }

    let record = Record {
        wifi_ssid: required(body, "wifi_ssid")?,
        wifi_password: field(body, "wifi_password")?.unwrap_or_default(),
        mqtt_host: required(body, "mqtt_host")?,
        mqtt_port,
        mqtt_user: field(body, "mqtt_user")?.unwrap_or_default(),
        mqtt_password: field(body, "mqtt_password")?.unwrap_or_default(),
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
        }
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

    // ---- the form ----

    #[test]
    fn a_submitted_form_becomes_a_record() {
        let body = "wifi_ssid=fdlgrm&wifi_password=hunter2\
                    &mqtt_host=192.168.68.108&mqtt_port=1883\
                    &mqtt_user=feeder&mqtt_password=feeder-dev";

        assert_eq!(record_from_form(body).unwrap(), sample());
    }

    #[test]
    fn form_encoding_is_decoded() {
        // A password of `p@ss word+1%` as a browser would send it.
        let body = "wifi_ssid=My+Network&wifi_password=p%40ss+word%2B1%25\
                    &mqtt_host=broker.local&mqtt_port=1883";
        let record = record_from_form(body).unwrap();

        assert_eq!(record.wifi_ssid.as_str(), "My Network");
        assert_eq!(record.wifi_password.as_str(), "p@ss word+1%");
    }

    #[test]
    fn a_multibyte_character_survives_decoding() {
        // Decoding has to assemble the bytes before checking UTF-8, or an
        // accented SSID comes back as an encoding error.
        let body = "wifi_ssid=Caff%C3%A8&mqtt_host=h&mqtt_port=1883";
        assert_eq!(record_from_form(body).unwrap().wifi_ssid.as_str(), "Caffè");
    }

    #[test]
    fn an_open_network_needs_no_password() {
        let body = "wifi_ssid=Open&wifi_password=&mqtt_host=h&mqtt_port=1883";
        let record = record_from_form(body).unwrap();

        assert!(record.wifi_password.is_empty());
        assert!(record.is_usable());
    }

    #[test]
    fn the_fields_that_cannot_be_blank_are_rejected() {
        assert_eq!(
            record_from_form("wifi_ssid=&mqtt_host=h&mqtt_port=1883"),
            Err(FormError::Blank("wifi_ssid"))
        );
        assert_eq!(
            record_from_form("wifi_ssid=s&mqtt_host=&mqtt_port=1883"),
            Err(FormError::Blank("mqtt_host"))
        );
        assert_eq!(
            record_from_form("mqtt_host=h&mqtt_port=1883"),
            Err(FormError::Missing("wifi_ssid"))
        );
    }

    #[test]
    fn a_bad_port_is_rejected_rather_than_defaulted() {
        // Quietly falling back to 1883 would hide a typo until the unit failed
        // to connect, with nothing on the form to show for it.
        for body in [
            "wifi_ssid=s&mqtt_host=h",
            "wifi_ssid=s&mqtt_host=h&mqtt_port=",
            "wifi_ssid=s&mqtt_host=h&mqtt_port=0",
            "wifi_ssid=s&mqtt_host=h&mqtt_port=99999",
            "wifi_ssid=s&mqtt_host=h&mqtt_port=1883x",
        ] {
            assert_eq!(record_from_form(body), Err(FormError::BadPort), "{body}");
        }
    }

    #[test]
    fn an_oversized_field_is_refused_not_truncated() {
        let long = core::iter::repeat_n('x', SSID_LEN + 1).collect::<std::string::String>();
        let body = std::format!("wifi_ssid={long}&mqtt_host=h&mqtt_port=1883");

        assert_eq!(
            record_from_form(&body),
            Err(FormError::TooLong("field")),
            "a truncated SSID would silently join the wrong network"
        );
    }

    #[test]
    fn a_broken_percent_escape_is_an_error() {
        for body in [
            "wifi_ssid=a%&mqtt_host=h&mqtt_port=1883",
            "wifi_ssid=a%4&mqtt_host=h&mqtt_port=1883",
            "wifi_ssid=a%zz&mqtt_host=h&mqtt_port=1883",
        ] {
            assert!(matches!(
                record_from_form(body),
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
