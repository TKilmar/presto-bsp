//! Settings in the last 4 KiB sector of the 16 MiB flash.

use embassy_rp::Peri;
use embassy_rp::flash::{Blocking, ERASE_SIZE, Flash};
use embassy_rp::peripherals::FLASH;
use nursery_core::settings::{STORED_SIZE, Settings};

const FLASH_SIZE: usize = 16 * 1024 * 1024;
const OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;

pub struct Storage {
    flash: Flash<'static, FLASH, Blocking, FLASH_SIZE>,
}

impl Storage {
    pub fn new(flash: Peri<'static, FLASH>) -> Self {
        Self { flash: Flash::new_blocking(flash) }
    }

    /// The stored settings, or the defaults if none were saved yet.
    pub fn load(&mut self) -> Settings {
        let mut buf = [0u8; STORED_SIZE];
        match self.flash.blocking_read(OFFSET, &mut buf) {
            Ok(()) => Settings::from_bytes(&buf).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    /// Store `settings` if they changed. Blocks for ~50 ms with interrupts off; the display
    /// keeps running because its scan-out only reads SRAM.
    pub fn save(&mut self, settings: &Settings) -> Result<(), embassy_rp::flash::Error> {
        if self.load() == *settings {
            return Ok(());
        }
        let mut page = [0xffu8; 256];
        page[..STORED_SIZE].copy_from_slice(&settings.to_bytes());
        self.flash.blocking_erase(OFFSET, OFFSET + ERASE_SIZE as u32)?;
        self.flash.blocking_write(OFFSET, &page)
    }
}
