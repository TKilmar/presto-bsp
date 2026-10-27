#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use presto_bsp::Presto;
use {defmt_rtt as _, panic_probe as _};


#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    
    presto_examples::init_heap();

    info!("Starting FT6236 touch demo");
    let board = Presto::default();
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
