//! The credentials record in flash: what a unit needs to reach the network
//! and to run its own mechanism.
//!
//! Pure logic, bytes in and bytes out; `store.rs` moves them. State, in
//! ADR-0028's layers.
//!
//! ## How a unit gets configured
//!
//! ```text
//!   boot ── read the record from the nvs partition
//!            ├── usable  → station mode, connect, run normally
//!            └── missing, or no network in it
//!                        → access point, serve the form, save, reboot
//!
//!   the BOOT hold → the record without its network, see `Record::without_network`
//! ```
//!
//! There is exactly one way into setup, which is why a reset rewrites the
//! record rather than signalling: "no usable record" is the only state the
//! boot path has to recognise.
//!
//! ## Why a checksum
//!
//! Erased flash reads back as `0xFF` everywhere, and a write interrupted by a
//! power cut leaves something halfway. Both must read as *no configuration*
//! rather than as garbage credentials, because a unit that believes a corrupt
//! record will sit trying to join a network that does not exist, with no way in
//! but the button. The magic catches blank flash and the CRC catches the rest.

use heapless::String;

use crate::calibrate::{DEFAULT_DETENT_MS, MIN_DETENT_MS};
use crate::crc32::crc32;

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

    /// This record with the network forgotten and the mechanism remembered:
    /// every credential blank, the calibration as it was.
    ///
    /// What the BOOT button held five seconds writes. The result is not
    /// [`Record::is_usable`], so the next boot goes to setup mode exactly as
    /// for erased flash; but it still decodes, so setup mode finds the
    /// calibration in it and carries it into the record it saves. Erasing the
    /// record instead used to
    /// lose the bench-measured detent interval along with the Wi-Fi password.
    ///
    /// Both passwords are blanked, not kept: a reset exists to forget the
    /// network, and the old Wi-Fi password is the last thing that should
    /// survive one.
    pub fn without_network(&self) -> Self {
        Self {
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            mqtt_host: String::new(),
            mqtt_port: 0,
            mqtt_user: String::new(),
            mqtt_password: String::new(),
            detent_ms: self.detent_ms,
            portion_scale_pct: self.portion_scale_pct,
        }
    }

    /// Whether a reset wrote this record: no network in it at all, only the
    /// calibration. Lets the boot path tell a deliberate reset from a record
    /// that is merely unusable, which reads like corruption.
    pub fn is_network_forgotten(&self) -> bool {
        *self == self.without_network()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        Record {
            wifi_ssid: String::try_from("MyWiFi").unwrap(),
            wifi_password: String::try_from("hunter2").unwrap(),
            mqtt_host: String::try_from("192.168.1.108").unwrap(),
            mqtt_port: 1883,
            mqtt_user: String::try_from("feeder").unwrap(),
            mqtt_password: String::try_from("feeder-dev").unwrap(),
            detent_ms: DEFAULT_DETENT_MS,
            portion_scale_pct: crate::portions::SCALE_UNCHANGED,
        }
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
}
