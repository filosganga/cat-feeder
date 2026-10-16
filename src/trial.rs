//! What setup mode found when it tried the network it was given, and how the
//! page says so.
//!
//! Setup does not save a record blind (ADR-0025). It joins the Wi-Fi while the
//! phone stays on the setup network, waits for an address, and logs in to the
//! broker; only a Wi-Fi that joined is ever saved. That matters because of
//! ADR-0005: once saved, a unit never falls back to setup by itself, so a
//! mistyped password used to mean a unit retrying forever until someone held
//! BOOT.
//!
//! The radio, the sockets and the waiting are `setup.rs`. What is here is the
//! outcome and its wording, which is pure and host-tested — the wording being
//! the part someone holding a phone actually reads.

use core::fmt::Write as _;

use heapless::String;

use crate::provisioning::Escaped;

/// Why the Wi-Fi was not joined.
///
/// Coarse on purpose. esp-radio reports some sixty disconnect reasons, and a
/// person can act on four; the reason itself goes to the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinFailure {
    /// The network answered and refused the password: a handshake timeout or
    /// an authentication failure, which is what a wrong WPA2 key looks like.
    WrongPassword,
    /// Nothing by that name answered, or nothing with a security this unit can
    /// use.
    NotFound,
    /// Joined, but no DHCP lease arrived in time.
    NoAddress,
    /// Anything else, or no answer at all within the time allowed.
    Other,
}

/// Why the broker did not accept a login, once the Wi-Fi had joined.
///
/// Returned by `mqtt::handshake`, so the normal session and the setup check
/// classify a failure the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerFailure {
    /// The TCP connection was reset: the address answers, nothing listens on
    /// the port.
    Refused,
    /// No answer to the TCP connect at all: wrong address, or the host is down.
    Unreachable,
    /// The broker answered CONNECT and refused the username or password.
    BadLogin,
    /// TCP connected and CONNECT went unanswered, or the answer was not MQTT.
    NoAnswer,
    /// The broker refused for some other reason; the console has the code.
    Other,
}

/// Where a trial is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trial {
    /// Nothing has been submitted yet.
    Idle,
    /// Joining, waiting for an address, or logging in to the broker.
    Testing,
    /// The Wi-Fi did not join. Nothing was saved.
    WifiFailed(JoinFailure),
    /// The Wi-Fi joined at `addr` and the broker did not accept the login.
    /// Nothing was saved, but *Save anyway* is offered: a unit may be set up
    /// before its broker exists, and the normal session retries it forever.
    BrokerFailed { addr: [u8; 4], why: BrokerFailure },
    /// Everything passed, and flash refused the write.
    NotSaved,
    /// Everything passed, the record is in flash, and the unit is about to
    /// restart. `addr` is where it was given an address.
    Saved { addr: [u8; 4] },
}

/// Whether `password` can be a Wi-Fi key at all: empty for an open network,
/// or a WPA passphrase of 8 to 63 characters, or 64 hex digits.
///
/// Checked before the radio is touched, not as a nicety: esp-radio's
/// `set_config` stops Wi-Fi altogether when the driver refuses a station
/// config, and that would take the setup network down under the phone that
/// sent it. A key the driver cannot take is reported as a wrong password,
/// which it is.
pub fn usable_key(password: &str) -> bool {
    match password.len() {
        0 | 8..=63 => true,
        64 => password.bytes().all(|b| b.is_ascii_hexdigit()),
        _ => false,
    }
}

/// Whether a press of *Test and save* starts a trial: not while one runs, and
/// not after one saved, when the unit is about to restart.
pub fn may_start(trial: Trial) -> bool {
    !matches!(trial, Trial::Testing | Trial::Saved { .. })
}

/// What *Save anyway* may keep: only a Wi-Fi that joined, with a broker that
/// did not, and the address it joined at. `None` from any other state.
pub fn keepable(trial: Trial) -> Option<[u8; 4]> {
    match trial {
        Trial::BrokerFailed { addr, .. } => Some(addr),
        _ => None,
    }
}

/// A sentence for the page: the longest is about two hundred bytes
/// before escaping, and an SSID may escape to six bytes a character.
pub const MESSAGE_LEN: usize = 1024;

/// What to tell the person, for a trial that failed. `None` for any other.
///
/// `ssid`, `host` and `port` are the ones that were tried, so the sentence
/// names them: "MyWiFi refused the password" is something to act on, and
/// "the password was refused" makes the reader wonder which one.
pub fn message(trial: Trial, ssid: &str, host: &str, port: u16) -> Option<String<MESSAGE_LEN>> {
    let mut m = String::new();
    let ssid = Escaped(ssid);
    let host = Escaped(host);
    let _ = match trial {
        Trial::WifiFailed(JoinFailure::WrongPassword) => write!(
            m,
            "<b>{ssid}</b> refused the password. Check it and try again; \
             it is case sensitive."
        ),
        Trial::WifiFailed(JoinFailure::NotFound) => write!(
            m,
            "No network called <b>{ssid}</b> answered. Check the name, which is \
             case sensitive, and that the feeder is in range. Only 2.4 GHz \
             networks work."
        ),
        Trial::WifiFailed(JoinFailure::NoAddress) => write!(
            m,
            "Joined <b>{ssid}</b>, but the router handed out no address. \
             Try again; if it keeps happening, check the router's DHCP."
        ),
        Trial::WifiFailed(JoinFailure::Other) => write!(
            m,
            "Could not join <b>{ssid}</b>. Try again; if it keeps failing, the \
             feeder's serial console says why."
        ),
        Trial::NotSaved => write!(
            m,
            "The settings for <b>{ssid}</b> could not be saved to flash. \
             Try again."
        ),
        Trial::BrokerFailed { why, .. } => {
            let _ = write!(m, "Joined <b>{ssid}</b>, but ");
            let _ = match why {
                BrokerFailure::Refused => {
                    write!(m, "nothing is listening at <b>{host}:{port}</b>.")
                }
                BrokerFailure::Unreachable => {
                    write!(m, "nothing answered at <b>{host}</b>.")
                }
                BrokerFailure::BadLogin => write!(
                    m,
                    "the broker at <b>{host}:{port}</b> refused the username \
                     or password."
                ),
                BrokerFailure::NoAnswer => write!(
                    m,
                    "<b>{host}:{port}</b> accepted the connection and never \
                     answered as a broker."
                ),
                BrokerFailure::Other => write!(
                    m,
                    "the broker at <b>{host}:{port}</b> refused the connection."
                ),
            };
            write!(
                m,
                " Fix it and test again, or save anyway: the feeder keeps \
                 retrying its broker."
            )
        }
        Trial::Idle | Trial::Testing | Trial::Saved { .. } => return None,
    };
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: [u8; 4] = [192, 168, 1, 42];

    fn says(trial: Trial) -> std::string::String {
        message(trial, "MyWiFi", "192.168.1.10", 1883)
            .expect("a failure has a message")
            .to_string()
    }

    #[test]
    fn a_wrong_password_names_the_network() {
        let m = says(Trial::WifiFailed(JoinFailure::WrongPassword));
        assert!(m.contains("<b>MyWiFi</b> refused the password"), "{m}");
    }

    #[test]
    fn a_missing_network_mentions_the_band() {
        // The C6 has no 5 GHz radio, and a 5 GHz-only SSID is the one cause
        // nobody guesses.
        let m = says(Trial::WifiFailed(JoinFailure::NotFound));
        assert!(m.contains("2.4 GHz"), "{m}");
    }

    #[test]
    fn a_broker_failure_names_the_broker_and_offers_saving_anyway() {
        for why in [
            BrokerFailure::Refused,
            BrokerFailure::BadLogin,
            BrokerFailure::NoAnswer,
            BrokerFailure::Other,
        ] {
            let m = says(Trial::BrokerFailed { addr: ADDR, why });
            assert!(m.contains("192.168.1.10:1883"), "{why:?}: {m}");
            assert!(m.contains("save anyway"), "{why:?}: {m}");
        }
        let m = says(Trial::BrokerFailed {
            addr: ADDR,
            why: BrokerFailure::Unreachable,
        });
        assert!(m.contains("192.168.1.10"), "{m}");
    }

    #[test]
    fn a_bad_login_is_told_apart_from_a_missing_broker() {
        let login = says(Trial::BrokerFailed {
            addr: ADDR,
            why: BrokerFailure::BadLogin,
        });
        let missing = says(Trial::BrokerFailed {
            addr: ADDR,
            why: BrokerFailure::Refused,
        });
        assert!(login.contains("username or password"), "{login}");
        assert!(missing.contains("nothing is listening"), "{missing}");
    }

    #[test]
    fn the_network_name_is_escaped() {
        let m = message(
            Trial::WifiFailed(JoinFailure::WrongPassword),
            "<b>&\"",
            "h",
            1,
        )
        .unwrap();
        assert!(m.starts_with("<b>&lt;b&gt;&amp;&quot;</b>"), "{m}");
    }

    #[test]
    fn the_longest_message_fits_with_every_character_escaped() {
        // An SSID of `"` escapes to six bytes a character, a host of `&` to
        // five; the message must come out whole, not cut mid-tag.
        let ssid = "\"".repeat(crate::provisioning::SSID_LEN);
        let host = "&".repeat(crate::provisioning::HOST_LEN);
        for trial in [
            Trial::WifiFailed(JoinFailure::NotFound),
            Trial::BrokerFailed {
                addr: ADDR,
                why: BrokerFailure::NoAnswer,
            },
        ] {
            let m = message(trial, &ssid, &host, 65535).unwrap();
            assert!(m.ends_with('.'), "{trial:?} was cut: {m}");
        }
    }

    #[test]
    fn a_key_the_driver_would_refuse_is_caught_first() {
        assert!(usable_key(""), "open network");
        assert!(usable_key("12345678"));
        assert!(usable_key(&"x".repeat(63)));
        assert!(usable_key(&"a1".repeat(32)), "a 64-digit hex PSK");
        assert!(!usable_key("1234567"), "WPA2 needs eight");
        assert!(!usable_key("a"));
        assert!(!usable_key(&"x".repeat(64)), "64 characters must be hex");
    }

    #[test]
    fn a_flash_failure_claims_nothing_about_the_broker() {
        // `NotSaved` is reached from *Save anyway* too, where the broker had
        // failed: the sentence must not say it worked.
        let m = says(Trial::NotSaved);
        assert!(m.contains("could not be saved to flash"), "{m}");
        assert!(!m.contains("broker"), "{m}");
    }

    #[test]
    fn a_trial_starts_unless_one_runs_or_one_saved() {
        assert!(may_start(Trial::Idle));
        assert!(may_start(Trial::WifiFailed(JoinFailure::NotFound)));
        assert!(may_start(Trial::NotSaved));
        assert!(may_start(Trial::BrokerFailed {
            addr: ADDR,
            why: BrokerFailure::BadLogin
        }));
        assert!(!may_start(Trial::Testing));
        assert!(!may_start(Trial::Saved { addr: ADDR }));
    }

    #[test]
    fn only_a_joined_wifi_with_a_failed_broker_can_be_kept() {
        let failed = Trial::BrokerFailed {
            addr: ADDR,
            why: BrokerFailure::Refused,
        };
        assert_eq!(keepable(failed), Some(ADDR));
        for trial in [
            Trial::Idle,
            Trial::Testing,
            Trial::WifiFailed(JoinFailure::WrongPassword),
            Trial::NotSaved,
            Trial::Saved { addr: ADDR },
        ] {
            assert_eq!(keepable(trial), None, "{trial:?}");
        }
    }

    #[test]
    fn only_a_failure_has_a_message() {
        for trial in [Trial::Idle, Trial::Testing, Trial::Saved { addr: ADDR }] {
            assert_eq!(message(trial, "s", "h", 1), None, "{trial:?}");
        }
    }
}
