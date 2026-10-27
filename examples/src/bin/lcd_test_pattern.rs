//! Display bring-up test: draws a pattern that exercises every data line, starts scan-out, then
//! redraws a block each second while the CPU hammers SRAM to check scan-out is unaffected.
//!
//! Expected picture: red increases left to right, green top to bottom, blue is an XOR checker,
//! a white 1-pixel border marks the exact panel edges, and a square in the top-left corner
//! alternates between black and white once per second.
//!
//! The `emu` harness at the repository root runs this binary in the RP2350 emulator and checks
//! the HSYNC/VSYNC/PCLK/data waveforms against the same pattern.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use defmt::info;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use presto_bsp::Presto;
use presto_bsp::lcd::{DISPLAY_HEIGHT, DISPLAY_WIDTH, FrameBuffer, Rgb565Pixel};
use {defmt_rtt as _, panic_probe as _};

/// Size of the toggling square in the top-left corner.
const BLOCK: usize = 64;

/// The reference pattern; the emulator harness has an identical copy.
fn pattern(x: usize, y: usize) -> u16 {
    if x == 0 || y == 0 || x == DISPLAY_WIDTH - 1 || y == DISPLAY_HEIGHT - 1 {
        return 0xffff;
    }
    let r = (x * 32 / DISPLAY_WIDTH) as u16;
    let g = (y * 64 / DISPLAY_HEIGHT) as u16;
    let b = if ((x / 30) ^ (y / 30)) & 1 == 1 { 0x1f } else { (x ^ y) as u16 & 0x1f };
    (r << 11) | (g << 5) | b
}

/// Frames shown so far; the emulator harness reads this through the ELF symbol.
#[unsafe(no_mangle)]
static FRAME_COUNT: AtomicU32 = AtomicU32::new(0);

#[embassy_executor::task]
async fn bus_hammer() {
    // Keep the CPU and SRAM busy so scan-out is tested under bus contention.
    let mut scratch = [0u32; 1024];
    let mut seed = 1u32;
    loop {
        for word in scratch.iter_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *word ^= seed;
        }
        embassy_futures::yield_now().await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    presto_examples::init_heap();

    let mut board = Presto::default();
    info!("panel initialised");

    let mut fb = FrameBuffer::take().unwrap();
    for (i, px) in fb.pixels_mut().iter_mut().enumerate() {
        *px = Rgb565Pixel(pattern(i % DISPLAY_WIDTH, i / DISPLAY_WIDTH));
    }
    board.lcd.start(&fb).unwrap();
    board.backlight.set_percent(80);
    info!("scan-out running");

    spawner.spawn(bus_hammer().unwrap());

    let mut white = false;
    loop {
        Timer::after(Duration::from_millis(1000)).await;
        board.lcd.wait_vblank().await;
        white = !white;
        let colour = Rgb565Pixel(if white { 0xffff } else { 0x0000 });
        for row in fb.pixels_mut().chunks_mut(DISPLAY_WIDTH).skip(1).take(BLOCK) {
            row[1..=BLOCK].fill(colour);
        }
        let frames = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
        info!("block toggled ({} toggles)", frames + 1);
    }
}
