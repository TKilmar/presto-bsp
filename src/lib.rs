#![no_std]
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod board;
pub use board::*;

/// LCD driver for the on-board ST7701 panel.
pub mod lcd;
/// PSRAM-backed display helpers.
pub mod display;
/// Simple PWM-based piezo speaker helper.
pub mod speaker;
/// FT6236 capacitive touch controller driver.
pub mod touch;

// Re-exports
pub use embassy_rp;
