#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

async fn pulse_pin(pin: &mut Output<'_>, count: u8) {
    for _ in 0..count {
        pin.set_high();
        Timer::after(Duration::from_millis(1)).await;
        pin.set_low();
        Timer::after(Duration::from_millis(1)).await;
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut gpio27 = Output::new(p.PIN_27, Level::Low);
    let mut gpio26 = Output::new(p.PIN_26, Level::Low);
    let mut gpio28 = Output::new(p.PIN_28, Level::Low);
    let mut gpio22 = Output::new(p.PIN_22, Level::Low);
    let mut gpio21 = Output::new(p.PIN_21, Level::Low);
    let mut gpio20 = Output::new(p.PIN_20, Level::Low);
    let mut gpio19 = Output::new(p.PIN_19, Level::Low);

    pulse_pin(&mut gpio27, 1).await;
    pulse_pin(&mut gpio26, 2).await;
    pulse_pin(&mut gpio28, 3).await;
    pulse_pin(&mut gpio22, 4).await;
    pulse_pin(&mut gpio21, 5).await;
    pulse_pin(&mut gpio20, 6).await;
    pulse_pin(&mut gpio19, 7).await;

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
