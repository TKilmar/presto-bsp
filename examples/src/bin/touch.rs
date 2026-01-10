#![no_std]
#![no_main]

extern crate alloc;

use defmt::*;
use embassy_executor::Spawner;
use presto_bsp::Presto;
use {defmt_rtt as _, panic_probe as _};


#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    
    // Simple bump allocator for Slint/alloc users.
    const HEAP_SIZE: usize = 200 * 1024;
    static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    #[global_allocator]
    static ALLOCATOR: embedded_alloc::Heap = embedded_alloc::Heap::empty();
    unsafe { ALLOCATOR.init(core::ptr::addr_of_mut!(HEAP) as usize, HEAP_SIZE) }

    info!("Starting FT6236 touch demo");
    let mut board = Presto::default();
    let mut touch = board.touch;

    loop {
        match touch.read_event().await {
            Ok(report) => {
                if let Some(p) = report.points[0] {
                    info!(
                        "touch {:?} at ({}, {}), gesture {:?}, touches {}",
                        if p.active { "active" } else { "inactive" },
                        p.x,
                        p.y,
                        report.gesture,
                        report.touches
                    );
                } else {
                    info!("touch gesture {:?}, touches {}", report.gesture, report.touches);
                }
            }
            Err(_) => warn!("touch read error"),
        }
    }
}
