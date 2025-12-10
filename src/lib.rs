#![no_std]
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod board;
pub use board::*;

pub mod lcd;
pub mod speaker;

// Re-exports
pub use embassy_rp;
