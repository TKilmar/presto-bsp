#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use presto_bsp::touch::Ft6236;
use presto_bsp::embassy_rp::{
    bind_interrupts,
    i2c::{self, InterruptHandler},
    peripherals::I2C0,
};
use presto_bsp::Presto;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    I2C0_IRQ => InterruptHandler<I2C0>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    info!("Starting FT6236 touch demo");
    let board = Presto::default();

    let i2c_config = i2c::Config::default();
    let i2c_bus = i2c::I2c::new_async(board.i2c0, board.i2c.scl, board.i2c.sda, Irqs, i2c_config);

    // The user button is used as a placeholder for the interrupt pin in this demo.
    // Replace `board.button` with the real FT6236 INT pin when connected.
    let mut touch = Ft6236::new(i2c_bus, board.button, false);

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
