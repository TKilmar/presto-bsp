#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use presto_bsp::lcd::{DISPLAY_HEIGHT, DISPLAY_WIDTH};
use presto_bsp::Presto;
use slint::platform::software_renderer as sr;
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Simple bump allocator for Slint/alloc users.
    const HEAP_SIZE: usize = 200 * 1024;
    static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    #[global_allocator]
    static ALLOCATOR: embedded_alloc::Heap = embedded_alloc::Heap::empty();
    unsafe { ALLOCATOR.init(core::ptr::addr_of_mut!(HEAP) as usize, HEAP_SIZE) }

    info!("LCD color cycle demo");

    let mut board = Presto::default();
    let mut lcd = board.lcd;

    // If you have a command transport hooked up, run panel init here.
    board.backlight.set_high();

    // Basic RGB565 colors.
    let colors = [0xF800u16, 0x07E0, 0x001F, 0xFFFF, 0x0000];

    // Create a buffer in PSRAM
    let base = board.psram.base_address() as *mut sr::Rgb565Pixel;
    let buffer = unsafe { core::slice::from_raw_parts_mut(base, DISPLAY_WIDTH * DISPLAY_HEIGHT) };

    loop {
        for &color in &colors {
            // Fill the raw framebuffer with the chosen color.
            unsafe {
                buffer.fill(slint::platform::software_renderer::Rgb565Pixel(color));
                let pixels: &mut [sr::Rgb565Pixel] = core::slice::from_raw_parts_mut(
                    buffer.as_mut_ptr() as *mut sr::Rgb565Pixel,
                    buffer.len(),
                );
                let _ = lcd.write_buffer(pixels).await;
                info!("New Color");
            }

            Timer::after(Duration::from_secs(1)).await;
        }
    }
}
