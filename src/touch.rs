//! FT6236 capacitive touch controller driver.
use embedded_hal::i2c::SevenBitAddress;
use embedded_hal_async::{digital::Wait, i2c::I2c};

/// Default 7-bit I2C address for the FT6236.
pub const TOUCH_I2C_ADDRESS: SevenBitAddress = 0x48;

/// FT6236 event codes.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum TouchEventCode {
    Down = 0b00,
    Up = 0b01,
    Contact = 0b10,
    None = 0b11,
}

impl TouchEventCode {
    fn is_active(self) -> bool {
        matches!(self, TouchEventCode::Down | TouchEventCode::Contact)
    }
}

impl From<u8> for TouchEventCode {
    fn from(value: u8) -> Self {
        match value & 0b11 {
            0b00 => TouchEventCode::Down,
            0b01 => TouchEventCode::Up,
            0b10 => TouchEventCode::Contact,
            _ => TouchEventCode::None,
        }
    }
}

/// High-level gesture identifiers.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum TouchGesture {
    /// No gesture detected.
    None = 0x00,
    /// Downward swipe.
    SlideDown = 0x01,
    /// Upward swipe.
    SlideUp = 0x02,
    /// Leftward swipe.
    SlideLeft = 0x03,
    /// Rightward swipe.
    SlideRight = 0x04,
    /// Single tap.
    SingleClick = 0x05,
    /// Double tap.
    DoubleClick = 0x0B,
    /// Long-press.
    LongPress = 0x0C,
}

impl From<u8> for TouchGesture {
    fn from(val: u8) -> Self {
        match val {
            0x01 => Self::SlideDown,
            0x02 => Self::SlideUp,
            0x03 => Self::SlideLeft,
            0x04 => Self::SlideRight,
            0x05 => Self::SingleClick,
            0x0B => Self::DoubleClick,
            0x0C => Self::LongPress,
            _ => Self::None,
        }
    }
}

/// Error type for the driver.
#[derive(Debug)]
pub enum TouchError<E> {
    /// I2C bus error.
    Bus(E),
    /// Interrupt pin error.
    Pin,
}

impl<E> From<E> for TouchError<E> {
    fn from(error: E) -> Self {
        TouchError::Bus(error)
    }
}

/// Touch point parsed from the controller report.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TouchPoint {
    /// Finger identifier reported by the controller.
    pub id: u8,
    /// X coordinate (scaled if requested).
    pub x: u16,
    /// Y coordinate (scaled if requested).
    pub y: u16,
    /// Whether the finger is currently active.
    pub active: bool,
}

/// Complete touch report.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TouchReport {
    /// Decoded gesture code.
    pub gesture: TouchGesture,
    /// Number of touches reported (may exceed provided slots).
    pub touches: u8,
    /// Up to two parsed touch points.
    pub points: [Option<TouchPoint>; 2],
}

/// FT6236 driver.
pub struct Ft6236<I2C, PINT> {
    i2c: I2C,
    pin_int: PINT,
    scale: u16,
}

impl<I2C, PINT, E> Ft6236<I2C, PINT>
where
    I2C: I2c<SevenBitAddress, Error = E>,
    PINT: Wait + embedded_hal::digital::InputPin,
{
    /// Create a new driver. Set `full_res` to `true` to keep the native 240x240 coordinate space,
    /// or `false` to scale down by 2 (matching the MicroPython example).
    pub fn new(bus: I2C, interrupt_pin: PINT, full_res: bool) -> Self {
        Self {
            i2c: bus,
            pin_int: interrupt_pin,
            scale: if full_res { 1 } else { 2 },
        }
    }

    /// Wait for the interrupt pin to fall, then read the latest touch report.
    pub async fn read_event(&mut self) -> Result<TouchReport, TouchError<E>> {
        self.pin_int
            .wait_for_falling_edge()
            .await
            .map_err(|_| TouchError::Pin)?;
        self.read_report().await
    }

    /// Poll the controller immediately (without waiting on the interrupt).
    pub async fn read_report(&mut self) -> Result<TouchReport, TouchError<E>> {
        let mut buf = [0u8; 15];
        self.i2c
            .write_read(TOUCH_I2C_ADDRESS, &[0x00], &mut buf)
            .await
            .map_err(TouchError::Bus)?;

        let gesture = TouchGesture::from(buf[1]);
        let touches = buf[2] & 0x0f;
        let mut points: [Option<TouchPoint>; 2] = [None, None];

        for n in 0..touches.min(2) {
            let offset = 3 + (n as usize) * 6;
            let event = TouchEventCode::from(buf[offset] >> 6);
            let x = (((buf[offset] & 0x0f) as u16) << 8 | buf[offset + 1] as u16) / self.scale;
            let y = (((buf[offset + 2] & 0x0f) as u16) << 8 | buf[offset + 3] as u16) / self.scale;
            let id = buf[offset + 2] >> 4;

            points[n as usize] = Some(TouchPoint {
                id,
                x,
                y,
                active: event.is_active(),
            });
        }

        Ok(TouchReport {
            gesture,
            touches,
            points,
        })
    }
}
