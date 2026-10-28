//! Platform-independent logic for the nursery display.
//!
//! Everything here is `no_std` and free of I/O so it runs unchanged on the Presto and in the
//! desktop simulator, and can be unit tested on the host (`cargo test` in this directory).
//!
//! * [`air`] – PM2.5 air-quality and CO₂ ventilation categories.
//! * [`sleepwear`] – baby sleepwear recommendations by room temperature (TOG ladder).
//! * [`history`] – a 30 h log of sensor readings and the overnight summary.
//! * [`forecast`] – outdoor forecast types and the "will it rain" summary.
//! * [`planner`] – the evening plan: expected overnight room temperatures.
//! * [`sensors`] – frame parsers for the PMSA003I and SCD4x sensors.
//! * [`clock`] – local wall-clock time and the morning/evening schedule.
//! * [`settings`] – user settings and their flash storage format.
//! * [`net`] – HTTP, location and forecast parsing for the Wi-Fi features.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod air;
pub mod clock;
pub mod forecast;
pub mod history;
pub mod net;
pub mod planner;
pub mod sensors;
pub mod settings;
pub mod sleepwear;
