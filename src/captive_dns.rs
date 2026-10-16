//! Pure logic for setup mode's DNS answerer: every name is this unit.
//!
//! A phone that joins the setup network asks for `connectivitycheck.gstatic.com`
//! or `captive.apple.com` before it does anything else. Answering every A query
//! with the unit's own address sends that probe to the setup form, and the
//! phone, seeing a page where it expected `204` or `Success`, opens it on its
//! own. That is the whole captive portal (ADR-0026): the address no longer has
//! to be typed in.
//!
//! The moving of bytes lives in [`crate::setup`]. What is here is the packet
//! and nothing else, so it is testable on the host.
//!
//! ## What is answered, and how
//!
//! - **A, class IN**: one answer, this unit's address, with a TTL of zero so no
//!   cache carries `192.168.4.1` onto the home network after setup.
//! - **Any other type** (AAAA, HTTPS, …): `NOERROR` with no answers. A refusal
//!   or a dropped query makes some phones wait for a timeout before falling
//!   back to A, which is seconds of nothing on a page someone is looking at.
//! - **Not answered at all**: a response, an opcode other than QUERY, anything
//!   but exactly one question, a compressed or over-long name, a truncated
//!   packet. A client asking those is not a phone looking for the internet.
//!
//! Additional records (an EDNS `OPT`, mostly) are dropped, not echoed: the
//! reply's `ARCOUNT` is zero, which every resolver accepts.

/// The well-known port.
pub const PORT: u16 = 53;

/// The header, in bytes.
const HEADER: usize = 12;

/// An answer as appended after the question: a pointer to the question's name,
/// type, class, TTL, length and four address bytes.
const ANSWER: usize = 16;

/// The longest name RFC 1035 allows, on the wire.
const NAME_MAX: usize = 255;

/// The longest label RFC 1035 allows. A length byte above it is a pointer
/// (`0b11` in the top bits) or reserved.
const LABEL_MAX: usize = 63;

/// A compression pointer to offset 12, where the question's name starts.
const QUESTION_NAME: [u8; 2] = [0xc0, HEADER as u8];

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;

/// Writes the reply to `query` into `out`, and says how long it is.
///
/// `None` means send nothing — see the module doc for which queries those are.
/// It also means `out` was too small, which it is not if it is as large as the
/// query plus [`ANSWER`]; the caller's buffer is a full datagram.
pub fn answer(query: &[u8], addr: [u8; 4], out: &mut [u8]) -> Option<usize> {
    if query.len() < HEADER {
        return None;
    }
    let flags = query[2];
    let is_response = flags & 0x80 != 0;
    let opcode = (flags >> 3) & 0x0f;
    if is_response || opcode != 0 || be16(query, 4) != 1 {
        return None;
    }

    let name_end = name_end(query, HEADER)?;
    let question_end = name_end + 4;
    if query.len() < question_end {
        return None;
    }
    let qtype = be16(query, name_end);
    let qclass = be16(query, name_end + 2);
    let answers: u16 = if qtype == TYPE_A && qclass == CLASS_IN {
        1
    } else {
        0
    };

    let len = question_end + ANSWER * usize::from(answers);
    if out.len() < len {
        return None;
    }

    // The header: the query's id, then QR and AA set, RD copied, RA set so a
    // stub resolver does not think it asked the wrong kind of server, and
    // RCODE 0. One question, `answers` answers, nothing else.
    out[0] = query[0];
    out[1] = query[1];
    out[2] = 0x80 | 0x04 | (flags & 0x01);
    out[3] = 0x80;
    out[4..6].copy_from_slice(&1u16.to_be_bytes());
    out[6..8].copy_from_slice(&answers.to_be_bytes());
    out[8..12].fill(0);

    // The question, as asked.
    out[HEADER..question_end].copy_from_slice(&query[HEADER..question_end]);

    if answers == 1 {
        let a = &mut out[question_end..len];
        a[0..2].copy_from_slice(&QUESTION_NAME);
        a[2..4].copy_from_slice(&TYPE_A.to_be_bytes());
        a[4..6].copy_from_slice(&CLASS_IN.to_be_bytes());
        a[6..10].copy_from_slice(&0u32.to_be_bytes());
        a[10..12].copy_from_slice(&4u16.to_be_bytes());
        a[12..16].copy_from_slice(&addr);
    }

    Some(len)
}

/// Where the name starting at `at` ends: the offset just past its zero label.
///
/// A query's name is never compressed — there is nothing earlier in the packet
/// to point at — so a pointer is refused rather than followed.
fn name_end(packet: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    loop {
        let len = usize::from(*packet.get(i)?);
        if len == 0 {
            let end = i + 1;
            return (end - at <= NAME_MAX).then_some(end);
        }
        if len > LABEL_MAX {
            return None;
        }
        i += 1 + len;
        if i - at > NAME_MAX {
            return None;
        }
    }
}

fn be16(packet: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([packet[at], packet[at + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: [u8; 4] = [192, 168, 4, 1];

    /// A query as a phone sends one: recursion desired, one question.
    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        q
    }

    fn reply(q: &[u8]) -> Option<Vec<u8>> {
        let mut out = [0u8; 512];
        answer(q, UNIT, &mut out).map(|n| out[..n].to_vec())
    }

    #[test]
    fn an_a_query_is_answered_with_the_unit() {
        let q = query("connectivitycheck.gstatic.com", TYPE_A);
        let r = reply(&q).unwrap();

        assert_eq!(&r[0..2], &[0x12, 0x34], "the query's id");
        assert_eq!(r[2] & 0x80, 0x80, "a response");
        assert_eq!(r[2] & 0x01, 0x01, "RD copied");
        assert_eq!(r[3] & 0x0f, 0, "NOERROR");
        assert_eq!(be16(&r, 4), 1, "one question");
        assert_eq!(be16(&r, 6), 1, "one answer");
        assert_eq!(be16(&r, 10), 0, "no additional records");
        assert_eq!(&r[HEADER..q.len()], &q[HEADER..], "the question as asked");
        assert_eq!(&r[r.len() - 4..], &UNIT);
        assert_eq!(r.len(), q.len() + ANSWER);
    }

    #[test]
    fn the_answer_points_at_the_question_and_is_never_cached() {
        let q = query("captive.apple.com", TYPE_A);
        let r = reply(&q).unwrap();
        let a = &r[q.len()..];
        assert_eq!(&a[0..2], &[0xc0, 0x0c], "a pointer to offset 12");
        assert_eq!(be16(a, 2), TYPE_A);
        assert_eq!(be16(a, 4), CLASS_IN);
        assert_eq!(&a[6..10], &[0, 0, 0, 0], "TTL zero");
        assert_eq!(be16(a, 10), 4);
    }

    #[test]
    fn every_name_is_the_unit() {
        for name in ["example.com", "a", "www.google.com", "cat-feeder.lan"] {
            let r = reply(&query(name, TYPE_A)).unwrap();
            assert_eq!(&r[r.len() - 4..], &UNIT, "{name}");
        }
    }

    #[test]
    fn other_types_get_an_empty_noerror_rather_than_silence() {
        // AAAA is asked alongside A by every phone, and HTTPS (65) by recent
        // ones. Silence would leave them waiting out a timeout.
        for qtype in [28, 65, 16] {
            let q = query("captive.apple.com", qtype);
            let r = reply(&q).unwrap();
            assert_eq!(r[3] & 0x0f, 0, "NOERROR for {qtype}");
            assert_eq!(be16(&r, 6), 0, "no answers for {qtype}");
            assert_eq!(r.len(), q.len());
        }
    }

    #[test]
    fn an_edns_record_is_dropped_not_echoed() {
        let mut q = query("example.com", TYPE_A);
        q[11] = 1; // ARCOUNT 1
        let question_len = q.len();
        // OPT: root name, type 41, payload size, ext rcode/flags, no data.
        q.extend_from_slice(&[0, 0, 41, 0x10, 0, 0, 0, 0, 0, 0, 0]);
        let r = reply(&q).unwrap();
        assert_eq!(be16(&r, 10), 0);
        assert_eq!(r.len(), question_len + ANSWER);
    }

    #[test]
    fn a_response_is_not_answered() {
        // Otherwise two of these on one network answer each other forever.
        let mut q = query("example.com", TYPE_A);
        q[2] |= 0x80;
        assert_eq!(reply(&q), None);
    }

    #[test]
    fn only_a_standard_query_is_answered() {
        let mut q = query("example.com", TYPE_A);
        q[2] |= 2 << 3; // STATUS
        assert_eq!(reply(&q), None);
    }

    #[test]
    fn anything_but_one_question_is_not_answered() {
        for count in [0u8, 2] {
            let mut q = query("example.com", TYPE_A);
            q[5] = count;
            assert_eq!(reply(&q), None, "{count} questions");
        }
    }

    #[test]
    fn a_truncated_packet_is_not_answered() {
        let q = query("example.com", TYPE_A);
        for len in [0, 5, HEADER, HEADER + 3, q.len() - 1] {
            assert_eq!(reply(&q[..len]), None, "cut at {len}");
        }
    }

    #[test]
    fn a_compressed_name_in_a_query_is_refused() {
        let mut q = query("example.com", TYPE_A);
        q[HEADER] = 0xc0;
        assert_eq!(reply(&q), None);
    }

    #[test]
    fn an_over_long_name_is_refused() {
        let label = "a".repeat(63);
        let name = [label.as_str(); 5].join(".");
        assert_eq!(reply(&query(&name, TYPE_A)), None);
    }

    #[test]
    fn a_buffer_too_small_for_the_answer_is_not_written_past() {
        let q = query("example.com", TYPE_A);
        let mut out = vec![0u8; q.len() + ANSWER - 1];
        assert_eq!(answer(&q, UNIT, &mut out), None);
    }
}
