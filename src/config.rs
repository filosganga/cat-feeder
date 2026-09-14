//! Build-time configuration.
//!
//! Values come from `cfg.toml`, injected as environment variables by
//! `build.rs`. They are compiled into the binary, so changing `cfg.toml`
//! requires a rebuild rather than only a reflash.
//!
//! Everything goes through [`load_config`] so that a later runtime-provisioning
//! version (captive portal, BLE) is a drop-in replacement for this module and
//! nothing else has to change.

use core::fmt::Write as _;

use esp_hal::efuse::{self, InterfaceMacAddress};
use heapless::String;

/// Wi-Fi and broker settings.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub wifi_ssid: &'static str,
    pub wifi_password: &'static str,
    pub mqtt_host: &'static str,
    pub mqtt_port: u16,
    pub mqtt_user: &'static str,
    pub mqtt_password: &'static str,
}

/// The configuration this firmware was built with.
pub const fn load_config() -> Config {
    Config {
        wifi_ssid: env!("CFG_WIFI_SSID"),
        wifi_password: env!("CFG_WIFI_PASSWORD"),
        mqtt_host: env!("CFG_MQTT_HOST"),
        mqtt_port: parse_u16(env!("CFG_MQTT_PORT")),
        mqtt_user: env!("CFG_MQTT_USER"),
        mqtt_password: env!("CFG_MQTT_PASSWORD"),
    }
}

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

/// Parses a decimal `u16` at compile time, so a bad port in `cfg.toml` fails
/// the build rather than the boot.
const fn parse_u16(text: &str) -> u16 {
    let bytes = text.as_bytes();
    assert!(!bytes.is_empty(), "mqtt_port must not be empty");

    let mut value: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let digit = bytes[i];
        assert!(
            digit >= b'0' && digit <= b'9',
            "mqtt_port must be decimal digits"
        );
        value = value * 10 + (digit - b'0') as u32;
        assert!(value <= u16::MAX as u32, "mqtt_port is out of range");
        i += 1;
    }

    value as u16
}
