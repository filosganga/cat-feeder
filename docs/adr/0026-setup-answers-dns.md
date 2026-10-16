# Setup answers every DNS name with itself

The setup network is a captive portal. DHCP hands out the unit as DNS server;
a tiny responder (`captive_dns.rs`) answers every A query with `192.168.4.1`
and every other type with an empty `NOERROR`; any path but the form's is
redirected to it. A phone's connectivity
probe then lands on the form and the phone opens it by itself, so joining from
the label's QR code is the whole procedure and nobody types an address.

It was left out at first as an extra. It is cheap — no crate, a few hundred
bytes of code — and setup is the one moment the unit's network has nothing
else to resolve. The address is still on the panel and the label, for a
browser that does not take the hint. TTLs are zero, so nothing cached on the
setup network outlives it. DHCP option 114 (RFC 8910) is not used: it names
an RFC 8908 API, expected over HTTPS and answering JSON, which a form on plain
HTTP is not.
