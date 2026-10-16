//! Just enough HTTP for the admin page and the setup form: a request's head,
//! and the fields of a form body.
//!
//! Pure logic, sans-IO; `http.rs` moves the bytes. A control's, in ADR-0028's
//! layers.

use heapless::String;

/// Why a form field could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldError {
    /// A field was longer than the caller can store.
    TooLong(&'static str),
    /// A percent-escape was malformed, or the result was not UTF-8.
    BadEncoding(&'static str),
}

// ---------------------------------------------------------------------------
// Form fields
// ---------------------------------------------------------------------------

/// Finds one field and percent-decodes it.
///
/// Returns `None` when the field is absent, which the caller distinguishes from
/// a field that is present and empty.
pub fn field<const N: usize>(
    body: &str,
    name: &'static str,
) -> Result<Option<String<N>>, FieldError> {
    match decoded(body, name)? {
        Some(value) => fit(&value, name).map(Some),
        None => Ok(None),
    }
}

/// Every field this firmware stores is far shorter than this, so decoding into
/// one size and narrowing afterwards costs a single stack buffer and buys the
/// ordering that `provisioning`'s `required` needs: **trim first, measure second**.
pub const DECODE_LEN: usize = 256;

/// Finds one field and percent-decodes it, without judging its length.
///
/// Split out from [`field`] so a value can be trimmed *before* it is measured.
/// Otherwise a 32-character SSID with the keyboard's trailing space is 33 bytes
/// and comes back as "too long" — refusing the one input the trimming exists to
/// rescue, with a message that names neither the field nor the space.
pub fn decoded(body: &str, name: &str) -> Result<Option<String<DECODE_LEN>>, FieldError> {
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
pub fn fit<const N: usize>(value: &str, name: &'static str) -> Result<String<N>, FieldError> {
    String::try_from(value).map_err(|_| FieldError::TooLong(name))
}

/// Percent-decoding, plus the `+` for space that form encoding adds.
fn decode_value(raw: &str) -> Result<String<DECODE_LEN>, FieldError> {
    // Decoded bytes first, because a multi-byte character arrives as several
    // escapes and only becomes valid UTF-8 once they are all decoded.
    let mut bytes: heapless::Vec<u8, DECODE_LEN> = heapless::Vec::new();
    let mut chars = raw.as_bytes().iter().copied();

    while let Some(byte) = chars.next() {
        let decoded = match byte {
            b'+' => b' ',
            b'%' => {
                let hi = chars.next().ok_or(FieldError::BadEncoding("percent"))?;
                let lo = chars.next().ok_or(FieldError::BadEncoding("percent"))?;
                hex(hi)? << 4 | hex(lo)?
            }
            other => other,
        };
        bytes
            .push(decoded)
            .map_err(|_| FieldError::TooLong("field"))?;
    }

    let text = core::str::from_utf8(&bytes).map_err(|_| FieldError::BadEncoding("utf-8"))?;
    String::try_from(text).map_err(|_| FieldError::TooLong("field"))
}

fn hex(byte: u8) -> Result<u8, FieldError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(FieldError::BadEncoding("percent")),
    }
}

// ---------------------------------------------------------------------------
// The head
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
    /// The `Authorization` header, or empty. One too long to be a credential
    /// this firmware issued is left empty too, which is a refusal.
    pub authorization: String<AUTH_LEN>,
    /// The `Host` header, or empty.
    pub host: String<64>,
    /// The `Origin` header, or empty — which is what `curl` and other
    /// non-browser clients send. An oversized one is an error rather than
    /// empty, because empty is what lets a request through the admin page's
    /// same-origin check.
    pub origin: String<64>,
}

/// Longest `Authorization` header kept: `Basic ` and the base64 of a
/// 64-character username and password pair, with room to spare.
pub const AUTH_LEN: usize = 128;

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
    let mut authorization = String::new();
    let mut host = String::new();
    let mut origin = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().map_err(|_| HttpError::Malformed)?;
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = String::try_from(value).unwrap_or_default();
        } else if name.eq_ignore_ascii_case("host") {
            host = String::try_from(value).map_err(|_| HttpError::TooLong)?;
        } else if name.eq_ignore_ascii_case("origin") {
            origin = String::try_from(value).map_err(|_| HttpError::TooLong)?;
        }
    }

    Ok(Some(Head {
        method,
        path: String::try_from(path).map_err(|_| HttpError::TooLong)?,
        content_length,
        body_at,
        authorization,
        host,
        origin,
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

    #[test]
    fn a_field_is_found_and_decoded() {
        let body = "a=1&ssid=My+Net%21&b=";
        assert_eq!(
            field::<16>(body, "ssid").unwrap().as_deref(),
            Some("My Net!")
        );
        assert_eq!(field::<16>(body, "b").unwrap().as_deref(), Some(""));
        assert_eq!(field::<16>(body, "missing").unwrap(), None);
        assert_eq!(field::<2>(body, "ssid"), Err(FieldError::TooLong("ssid")));
        assert!(matches!(
            field::<16>("x=%zz", "x"),
            Err(FieldError::BadEncoding(_))
        ));
    }

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

    #[test]
    fn the_headers_the_admin_page_checks_are_kept() {
        let head = parse_head(
            b"POST /schedule HTTP/1.1\r\nHost: 192.168.1.60\r\n\
              origin: http://192.168.1.60\r\nAUTHORIZATION: Basic YTpi\r\n\r\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(head.host, "192.168.1.60");
        assert_eq!(head.origin, "http://192.168.1.60");
        assert_eq!(head.authorization, "Basic YTpi");

        let bare = parse_head(b"GET / HTTP/1.1\r\n\r\n").unwrap().unwrap();
        assert!(bare.host.is_empty() && bare.origin.is_empty() && bare.authorization.is_empty());
    }

    /// Empty is what passes the same-origin check, so an Origin too long to
    /// hold must not quietly become empty.
    #[test]
    fn an_oversized_origin_is_refused_not_dropped() {
        let raw = std::format!(
            "POST / HTTP/1.1\r\nOrigin: http://{}\r\n\r\n",
            "x".repeat(80)
        );
        assert_eq!(parse_head(raw.as_bytes()), Err(HttpError::TooLong));

        let raw = std::format!(
            "GET / HTTP/1.1\r\nAuthorization: Basic {}\r\n\r\n",
            "A".repeat(200)
        );
        assert!(
            parse_head(raw.as_bytes())
                .unwrap()
                .unwrap()
                .authorization
                .is_empty()
        );
    }
}
