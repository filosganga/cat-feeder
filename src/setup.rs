//! Setup mode: the unit raises its own network and asks to be configured.
//!
//! Entered from the boot path when there is no usable record in flash, and
//! never left except by rebooting — see [`run`]. The decisions about *what* a
//! record contains and how a form maps onto one live in
//! [`crate::provisioning`], which is pure and host-tested; this module is the
//! radio, the sockets and the HTML.
//!
//! ## The one way in
//!
//! "No usable record" is the only state that reaches here. The reset button
//! erases rather than signalling, so there is no second path and no flag to get
//! out of step. There is deliberately **no fall back into setup after failing
//! to connect**: a router rebooting for five minutes must not drop a working
//! feeder into setup mode and stop it feeding.
//!
//! ## And no timeout
//!
//! A unit stays here until somebody configures it. Rebooting out would only
//! return here, and a unit that gives up while you are fetching your phone is
//! worse than one that waits.
//!
//! ## Built in slices
//!
//! Each slice is verifiable on its own, because the failure modes are
//! completely different: a network that will not appear is a radio problem, a
//! phone that joins and gets no address is DHCP, and a page that will not load
//! is the socket loop.
//!
//! 1. ✅ raise the access point — the SSID appears in a phone's Wi-Fi list
//! 2. ⬜ a second network stack on it, DHCP, so a phone gets an address
//! 3. ⬜ the form on TCP 80

use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{
    AuthenticationMethod, Config as WifiConfig, ControllerConfig, WifiController,
    ap::AccessPointConfig,
};
use log::{error, info};

use crate::provisioning::{ap_password, ap_ssid};

/// Raises this unit's setup network and stays there.
///
/// Never returns. Once slice 3 lands it reboots after saving a record, so the
/// normal boot path always starts clean rather than from a half-configured
/// process.
///
/// The controller is kept alive for the life of setup mode: dropping it takes
/// the network down, and a phone mid-form would simply lose its connection.
pub async fn run(wifi: WIFI<'static>, id: &str, secret: &str) -> ! {
    let ssid = ap_ssid(id);
    let password = ap_password(secret, id);

    // Printed in full, on purpose. This is a unit that by definition has no
    // other way to tell anyone its password, and a console is the one channel
    // that exists before the network does. `dev/ap-password.sh <id>` prints the
    // same string off the device, for stickers.
    info!("setup: raising {ssid}");
    info!("setup: password {password}");
    info!("setup: then browse to http://192.168.4.1");

    let config = AccessPointConfig::default()
        .with_ssid(ssid.as_str())
        // **Not optional.** `AccessPointConfig::default()` is an *open*
        // network, so leaving this out would broadcast a setup portal anyone
        // can join — and the session it protects is the one where the home
        // Wi-Fi password gets typed in. The salted password is worthless
        // without it.
        .with_auth_method(AuthenticationMethod::Wpa2Personal)
        .with_password(password.as_str().into());

    // No separate start call: `set_config` calls `esp_wifi_start()` whenever
    // the mode changes, so applying this as the initial config is what brings
    // the network up.
    let (controller, _interfaces) = match esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(WifiConfig::AccessPoint(config)),
    ) {
        Ok(pair) => pair,
        Err(e) => {
            // Nothing useful is left: no network, and no credentials to fall
            // back on. Say so loudly and stop, rather than looping on something
            // that will not start.
            error!("setup: could not raise the access point ({e:?})");
            halt().await
        }
    };

    info!("setup: access point up");

    hold(controller).await
}

/// Keeps the controller alive and the network up.
///
/// Slices 2 and 3 replace this with the DHCP and HTTP loops. Until then it is
/// what makes the SSID observable on a phone, which is the whole of slice 1.
async fn hold(_controller: WifiController<'static>) -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}

/// Stops, without spinning.
async fn halt() -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
