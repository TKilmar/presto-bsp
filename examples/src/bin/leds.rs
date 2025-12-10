#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use presto_bsp::{Presto, BOARD_LED_COUNT};
use smart_leds::RGB8;
use {defmt_rtt as _, panic_probe as _};

/// Input a value 0 to 255 to get a color value
/// The colours are a transition r - g - b - back to r.
fn wheel(mut wheel_pos: u8) -> RGB8 {
    wheel_pos = 255 - wheel_pos;
    if wheel_pos < 85 {
        return (255 - wheel_pos * 3, 0, wheel_pos * 3).into();
    }
    if wheel_pos < 170 {
        wheel_pos -= 85;
        return (0, wheel_pos * 3, 255 - wheel_pos * 3).into();
    }
    wheel_pos -= 170;
    (wheel_pos * 3, 255 - wheel_pos * 3, 0).into()
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    info!("Start LEDs");
    let board = Presto::default();

    let mut ws2812 = board.into_board_ws2812();
    let mut data = [RGB8::default(); BOARD_LED_COUNT];

    let mut ticker = Ticker::every(Duration::from_millis(20));
    loop {
        for j in 0..(256 * 5) {
            for i in 0..BOARD_LED_COUNT {
                data[i] = wheel((((i * 256) as u16 / BOARD_LED_COUNT as u16 + j as u16) & 255) as u8);
            }
            ws2812.write(&data).await;

            ticker.next().await;
        }
    }
}
