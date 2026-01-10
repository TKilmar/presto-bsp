use core::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use embassy_rp::{pio::Instance, psram::Psram};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::Channel,
};
use slint::platform::software_renderer as sr;

use crate::lcd::{St7701, DISPLAY_HEIGHT, DISPLAY_WIDTH};

/// Errors that can occur while setting up the display buffers.
#[derive(Debug)]
pub enum DisplayError {
    /// PSRAM is not large enough to hold two full-screen RGB565 frame buffers.
    PsramTooSmall,
}

/// Helper that wires an ST7701 driver to two frame buffers placed in PSRAM.
///
/// Two buffers are reserved so callers can render into one while the other is being
/// pushed to the panel.
pub struct Display<'d, PIO: Instance, const SM: usize> {
    /// Underlying ST7701 transport/streaming driver.
    pub lcd: St7701<'d, PIO, SM>,
    /// Front buffer for scanning out.
    pub front: &'d mut [sr::Rgb565Pixel],
    /// Back buffer for rendering.
    pub back: &'d mut [sr::Rgb565Pixel],
    swap_pending: AtomicBool,
    swap_req: Channel<NoopRawMutex, (), 1>,
}

impl<'d, PIO: Instance, const SM: usize> Display<'d, PIO, SM> {
    /// Create a new double-buffered display using PSRAM.
    ///
    /// The `psram` slice is divided into two equal RGB565 buffers of size
    /// `DISPLAY_WIDTH * DISPLAY_HEIGHT`. Returns an error if PSRAM is too small.
    pub fn new(lcd: St7701<'d, PIO, SM>, psram: &'d Psram<'d>) -> Result<Self, DisplayError> {
        let pixels = DISPLAY_WIDTH * DISPLAY_HEIGHT;
        let buf_bytes = pixels * mem::size_of::<sr::Rgb565Pixel>();
        if buf_bytes * 2 > psram.size() {
            return Err(DisplayError::PsramTooSmall);
        }

        let base = psram.base_address() as *mut sr::Rgb565Pixel;
        // Safety: PSRAM is memory-mapped and large enough for both buffers; pointers are aligned.
        let front = unsafe { core::slice::from_raw_parts_mut(base, pixels) };
        let back = unsafe { core::slice::from_raw_parts_mut(base.add(pixels), pixels) };

        Ok(Self {
            lcd,
            front,
            back,
            swap_pending: AtomicBool::new(false),
            swap_req: Channel::new(),
        })
    }

    /// Returns the current work buffer (not on screen).
    pub fn work_buffer(&mut self) -> &mut [sr::Rgb565Pixel] {
        self.back
    }

    /// Check if a swap is already in progress.
    pub fn is_swap_pending(&self) -> bool {
        self.swap_pending.load(Ordering::Relaxed)
    }

    /// Request that the display task present the work buffer.
    pub fn request_swap(&self) {
        if self
            .swap_pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = self.swap_req.try_send(());
        }
    }

    /// Background task: waits for swap requests, pushes the work buffer to the panel,
    /// then flips front/back.
    pub async fn task(&mut self) -> Result<(), DisplayError> {
        loop {
            let _ = self.swap_req.receive().await;
            // Send back buffer to panel.
            self.lcd.write_buffer(self.back).await.map_err(|_| DisplayError::PsramTooSmall)?;
            // Swap the buffer references for next frame.
            mem::swap(&mut self.front, &mut self.back);
            self.swap_pending.store(false, Ordering::SeqCst);
        }
    }
}
