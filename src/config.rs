//! What this unit runs on.
//!
//! [`Config`] comes from the record in flash and nowhere else. There is no
//! build-time fallback: network credentials are not compiled into this binary,
//! they are written to the `nvs` partition by `dev/provision.sh` or by the
//! setup form. See *Credentials: getting them out of the binary* in CLAUDE.md.
//!
//! The one build-time value left is [`AP_SECRET`], which salts the setup
//! network's password. It is not a credential for any network — it exists so
//! the firmware and `dev/ap-password.sh` derive the same per-unit string, and
//! there is nowhere else both could read it from.

use core::fmt::Write as _;

use esp_hal::efuse::{self, InterfaceMacAddress};

use heapless::String;

use crate::feeder::Timings;
use crate::provisioning::Record;

/// Wi-Fi and broker settings.
///
/// Borrowed rather than owned so this stays cheap to copy into the async task
/// frames that carry it. The strings live either in the binary, for a
/// build-time config, or in the [`Record`] the boot path leaks into a static
/// after reading it from flash.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub wifi_ssid: &'static str,
    pub wifi_password: &'static str,
    pub mqtt_host: &'static str,
    pub mqtt_port: u16,
    pub mqtt_user: &'static str,
    pub mqtt_password: &'static str,
    /// This unit's mechanical timings, derived from its measured detent
    /// interval. See `feeder::Timings`.
    pub timings: Timings,
    /// How much this unit dispenses per click. See `portions::clicks_for`.
    pub portion_scale_pct: u16,
}

impl Config {
    /// The settings a provisioned unit stored in flash.
    pub fn from_record(record: &'static Record) -> Self {
        Self {
            wifi_ssid: record.wifi_ssid.as_str(),
            wifi_password: record.wifi_password.as_str(),
            mqtt_host: record.mqtt_host.as_str(),
            mqtt_port: record.mqtt_port,
            mqtt_user: record.mqtt_user.as_str(),
            mqtt_password: record.mqtt_password.as_str(),
            // Through the accessors, which clamp. A nonsensical figure must not
            // stop a unit reaching the broker — that would leave the button as
            // the only way to re-provision it.
            timings: Timings::from_detent(record.detent_ms()),
            portion_scale_pct: record.portion_scale_pct(),
        }
    }
}

/// Salts the setup network's password. Not a network credential.
pub const AP_SECRET: &str = env!("CFG_AP_SECRET");

/// Number of characters in a device id.
pub const DEVICE_ID_LEN: usize = 6;

/// The device id used in every MQTT topic: the last three bytes of the Wi-Fi
/// station MAC as lowercase hex, for example `db0260`.
///
/// One binary flashes all three units, so this is the only thing that tells
/// them apart. Keep the derivation here and nowhere else.
pub fn device_id() -> String<DEVICE_ID_LEN> {
    let mac = efuse::interface_mac_address(InterfaceMacAddress::Station);
    let bytes = mac.as_bytes();

    let mut id = String::new();
    // Cannot fail: three bytes as hex is exactly DEVICE_ID_LEN characters.
    let _ = write!(id, "{:02x}{:02x}{:02x}", bytes[3], bytes[4], bytes[5]);
    id
}
