# Presto BSP

Board support package for the Pimoroni Presto (RP2350) built on top of
[`embassy-rp`](https://docs.rs/embassy-rp). It wires up the on-board
peripherals so applications can focus on their logic instead of pin maps.
Inspired by the excellent [`microbit-bsp`](https://github.com/lulf/microbit-bsp) project.

## Dependencies

- Install [`probe-rs`](https://probe.rs/) tooling (e.g. `cargo install probe-rs-tools`) for flashing and debugging.
- A Pimoroni Presto board to run the examples and firmware.

## Provided mappings

- `GPIO33` – SK6812 LED strip data (7 LEDs on board)
- `GPIO43` – Piezo speaker
- `GPIO34-39` – SD card interface (CLK, CMD, D0..D3)
- `GPIO45` – AD3031 LCD backlight enable
- `GPIO46` – User button
- `GPIO40/41` – I2C bus for add-ons
- `PIO0/PIO1` and `DMA_CH0/1` are exposed for driving the LEDs or other
  high-speed tasks

An LCD is present on the board, but only a placeholder is provided for now.

## Next steps

Planned functionality to flesh out next:
- SD card support
- LCD driver and examples
- Wi-Fi connectivity
- BLE connectivity

## Quick start

```rust
#![no_std]
#![no_main]

use {defmt_rtt as _, panic_probe as _};

use presto_bsp::Presto;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::{Duration, Timer};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let board = Presto::default();

    // Toggle the backlight to confirm everything is alive.
    let mut backlight = board.backlight;
    loop {
        backlight.set_high();
        Timer::after(Duration::from_millis(500)).await;
        backlight.set_low();
        Timer::after(Duration::from_millis(500)).await;
    }
}
```

## Cargo features

- `defmt` (default) – enable logging support in the dependencies.
