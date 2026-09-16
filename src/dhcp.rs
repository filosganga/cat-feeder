//! Pure logic for the DHCP server in setup mode: where a reply must be sent,
//! and how a MAC address is spelled on the console.
//!
//! The moving of bytes lives in [`crate::setup`], which is gated on the board.
//! What is here needs no socket and no clock, so it is testable on the host —
//! and worth testing, because two of its three branches cannot be reached with
//! a phone on a bench at all.
//!
//! ## Why a reply is not simply sent back where it came from
//!
//! A client asking for its first address does not have one yet. It cannot
//! receive a unicast, because it has no address to receive it at and no socket
//! bound to one, so the reply has to be broadcast even though the request
//! arrived from a specific station. RFC 2131 §4.1 sets out when that is not
//! the case, and [`reply_to`] is those rules and nothing else.

use core::net::Ipv4Addr;

/// The BOOTP ports. `edge-dhcp` only names these behind its `io` feature,
/// which is off in this project — see `Cargo.toml`, this is the codec only.
pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;

/// What a client's request says about how to reach it.
///
/// Taken apart into plain fields rather than passing `edge_dhcp::Packet`,
/// because that type lives behind the target gate and this module must build
/// for the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Via {
    /// The relay agent that forwarded this, or unspecified for a direct one.
    pub giaddr: Ipv4Addr,
    /// The address the client says it already holds, or unspecified.
    pub ciaddr: Ipv4Addr,
    /// The client set the broadcast flag: it is telling us it cannot receive a
    /// unicast yet.
    pub broadcast: bool,
    /// This reply is a NAK — a refusal carrying no address.
    pub nak: bool,
}

/// Where to send the reply, per RFC 2131 §4.1 and §4.3.2.
///
/// The order is the order the spec puts it in, and each branch is a different
/// kind of client:
///
/// 1. **Relayed.** Answer the relay, on the *server* port, and let it deliver.
///    Cannot happen on a network with one access point and no router, but it
///    costs nothing to be right.
/// 2. **Renewing.** The client told us a `ciaddr` it already holds, and did not
///    set the broadcast flag, so it can receive a unicast there. Note this is
///    `ciaddr` — what the client says it *has* — and never `yiaddr`, which it
///    does not have yet and has nothing listening on.
/// 3. **Everyone else.** Broadcast. This is the common case and the one a phone
///    joining for the first time takes.
///
/// A **NAK is always broadcast** when it is not relayed (§4.3.2), whatever the
/// client said about itself. A refusal usually means the two disagree about
/// where the client is, so its `ciaddr` is precisely the thing not to trust.
pub fn reply_to(via: Via) -> (Ipv4Addr, u16) {
    if !via.giaddr.is_unspecified() {
        (via.giaddr, SERVER_PORT)
    } else if via.nak {
        (Ipv4Addr::BROADCAST, CLIENT_PORT)
    } else if !via.ciaddr.is_unspecified() && !via.broadcast {
        (via.ciaddr, CLIENT_PORT)
    } else {
        (Ipv4Addr::BROADCAST, CLIENT_PORT)
    }
}

/// A MAC address, printed the way every other tool prints one.
///
/// `[u8; 6]` has no `Display`, and hand-rolling six `{:02x}` at each call site
/// is how the two that exist drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    /// The client hardware address out of a BOOTP packet's 16-byte `chaddr`.
    ///
    /// Only the first six bytes are an Ethernet MAC; the rest is padding.
    /// `edge-dhcp` refuses to decode a packet whose `hlen` is not 6, so a
    /// shorter slice cannot reach here — but this takes the whole array rather
    /// than relying on that, so there is no unwrap to be wrong about.
    pub fn from_chaddr(chaddr: [u8; 16]) -> Self {
        let [a, b, c, d, e, f, ..] = chaddr;
        Self([a, b, c, d, e, f])
    }
}

impl core::fmt::Display for Mac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOWHERE: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

    fn fresh() -> Via {
        Via {
            giaddr: NOWHERE,
            ciaddr: NOWHERE,
            broadcast: false,
            nak: false,
        }
    }

    #[test]
    fn a_client_with_no_address_is_answered_by_broadcast() {
        // The case a phone joining for the first time takes, and the only one
        // the bench has ever exercised.
        assert_eq!(reply_to(fresh()), (Ipv4Addr::BROADCAST, CLIENT_PORT));
    }

    #[test]
    fn a_renewing_client_is_answered_where_it_says_it_lives() {
        let held = Ipv4Addr::new(192, 168, 4, 2);
        let via = Via {
            ciaddr: held,
            ..fresh()
        };
        assert_eq!(reply_to(via), (held, CLIENT_PORT));
    }

    #[test]
    fn the_broadcast_flag_outranks_a_client_address() {
        // The client holds an address but is telling us it cannot receive a
        // unicast yet. Believe it.
        let via = Via {
            ciaddr: Ipv4Addr::new(192, 168, 4, 2),
            broadcast: true,
            ..fresh()
        };
        assert_eq!(reply_to(via), (Ipv4Addr::BROADCAST, CLIENT_PORT));
    }

    #[test]
    fn a_relayed_request_goes_back_to_the_relay_on_the_server_port() {
        let relay = Ipv4Addr::new(10, 0, 0, 1);
        let via = Via {
            giaddr: relay,
            ciaddr: Ipv4Addr::new(192, 168, 4, 2),
            ..fresh()
        };
        assert_eq!(reply_to(via), (relay, SERVER_PORT));
    }

    #[test]
    fn a_relay_outranks_even_a_nak() {
        // §4.3.2 sends a relayed NAK to the relay, not to the broadcast
        // address, so the relay can set the broadcast bit itself.
        let relay = Ipv4Addr::new(10, 0, 0, 1);
        let via = Via {
            giaddr: relay,
            nak: true,
            ..fresh()
        };
        assert_eq!(reply_to(via), (relay, SERVER_PORT));
    }

    #[test]
    fn a_nak_is_broadcast_even_to_a_client_that_claims_an_address() {
        // The refusal this exists for: a phone remembering 192.168.4.57 from
        // some other ESP access point, renewing it against ours. We disagree
        // with it about where it lives, so its own `ciaddr` is the last thing
        // to trust.
        let via = Via {
            ciaddr: Ipv4Addr::new(192, 168, 4, 57),
            nak: true,
            ..fresh()
        };
        assert_eq!(reply_to(via), (Ipv4Addr::BROADCAST, CLIENT_PORT));
    }

    #[test]
    fn the_two_ports_are_not_the_same_one() {
        // Answering a client on 67 is a silent failure: the packet goes out
        // and nothing is listening.
        assert_ne!(SERVER_PORT, CLIENT_PORT);
    }

    #[test]
    fn a_mac_is_spelled_the_way_every_other_tool_spells_one() {
        // Lower case, colon separated, leading zeros kept — so a console line
        // can be matched against what a phone's Wi-Fi settings show.
        let mac = Mac([0xea, 0xce, 0x1a, 0x6f, 0x94, 0x0b]);
        assert_eq!(format!("{mac}"), "ea:ce:1a:6f:94:0b");
    }

    #[test]
    fn a_mac_keeps_its_leading_and_trailing_zeros() {
        let mac = Mac([0x00, 0x01, 0x00, 0xff, 0x00, 0x00]);
        assert_eq!(format!("{mac}"), "00:01:00:ff:00:00");
    }

    #[test]
    fn only_the_first_six_bytes_of_chaddr_are_the_address() {
        // BOOTP carries 16 bytes and pads. Including the padding would print a
        // MAC that matches nothing on the phone.
        let mut chaddr = [0xaa; 16];
        chaddr[..6].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x11]);
        assert_eq!(format!("{}", Mac::from_chaddr(chaddr)), "de:ad:be:ef:00:11");
    }
}
