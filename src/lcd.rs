//! Placeholder for the on-board LCD.
//!
//! The Presto ships with an LCD connected separately from the backlight (which
//! is exposed on GPIO45). This module provides a small marker type that can be
//! swapped out for a concrete driver once the panel wiring and controller are
//! finalised.

/// Marker for the LCD connection.
#[derive(Clone, Copy, Debug)]
pub struct LcdPlaceholder;

impl LcdPlaceholder {
    /// Create a new placeholder instance.
    pub const fn new() -> Self {
        Self
    }
}
