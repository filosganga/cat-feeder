//! cat-feeder firmware.
//!
//! The crate is split so the parts worth testing can be tested.
//!
//! Pure-logic modules compile for any target and carry their own `#[cfg(test)]`
//! tests, which run on the host with `cargo test`. Modules that touch esp-hal
//! are gated on `target_os = "none"` so a host build simply leaves them out.
//!
//! Host tests need an explicit target, because `.cargo/config.toml` points
//! cargo at the board by default:
//!
//! ```sh
//! cargo test --lib --target "$(rustc -vV | awk '/^host:/{print $2}')"
//! ```
//!
//! Plain `cargo test` tries to build the tests for the ESP32-C6 and fails to
//! link; the target is not optional.

#![cfg_attr(not(test), no_std)]

// Pure logic. No esp-hal, testable on the host.
pub mod feeder;
pub mod portions;

// Hardware. Only built for the board.
#[cfg(target_os = "none")]
pub mod board;
#[cfg(target_os = "none")]
pub mod config;
#[cfg(target_os = "none")]
pub mod motor;
#[cfg(target_os = "none")]
pub mod mqtt;
#[cfg(target_os = "none")]
pub mod switch;
