# Presto BSP

Board support package for the Pimoroni Presto (RP2350B) built on
[`embassy-rp`](https://docs.rs/embassy-rp). It wires up the on-board peripherals so
applications can focus on their logic instead of pin maps. Inspired by
[`microbit-bsp`](https://github.com/lulf/microbit-bsp).

## What's included

- **480x480 LCD (ST7701)**: panel initialisation plus continuous scan-out driven entirely by
  PIO1 and four chained DMA channels. No CPU core is spent on the display (see below).
- **FT6236 touch** on its own I2C bus, in native 480x480 coordinates.
- **Backlight** PWM with perceptual brightness steps.
- **PSRAM** (8 MiB APS6404L), returned as `Option` so a missing chip is not fatal.
- **SK6812 LEDs**, **piezo speaker**, **user button**, **Qwiic/STEMMA QT** I2C, SD card pins.

## Pin and resource map

| Resource | Use |
| --- | --- |
| GPIO1–16 | LCD RGB565 data, MSB on GPIO1 (PIO1) |
| GPIO17/18 | LCD 18-bit LSBs, held low |
| GPIO19/20/21/22 | LCD HSYNC / VSYNC / DE (held high) / dot clock |
| GPIO26/27/28, GPIO44 | LCD command SPI1 CLK / MOSI / CS, panel reset |
| GPIO30/31/32, GPIO42 | Touch I2C1 SDA / SCL / interrupt, touch reset |
| GPIO33 | SK6812 LED strip (7 LEDs, PIO0 + DMA0) |
| GPIO34–39 | SD card |
| GPIO40/41 | Qwiic I2C0 SDA / SCL |
| GPIO43 | Piezo speaker |
| GPIO45 | Backlight enable (PWM) |
| GPIO46 | User button |
| GPIO47 | PSRAM chip select |
| PIO1, DMA1–4 | LCD scan-out |
| PIO0, DMA0 | LEDs (`board.leds.into_ws2812()`) |
| PIO2, GPIO23/24/25/29 | RM2 Wi-Fi module (`board.wifi`), e.g. with `cyw43-pio` |
| DMA5–7 | Free, exposed as `board.dma` |

## The display

The panel has no frame memory: it needs a complete frame streamed to it continuously, with
HSYNC/VSYNC timing locked to the dot clock. `St7701::start` sets that up once and then runs
without the CPU:

- PIO1 SM0 generates the dot clock, HSYNC and VSYNC from a small per-line descriptor table.
- PIO1 SM1 shifts out one line of pixels each time SM0 signals it. Both state machines share a
  clock divider and start on the same cycle, so the pixel phase cannot drift.
- DMA channel 1 streams the frame buffer into SM1. When it finishes, it chains to channel 2,
  which writes the frame buffer address back into channel 1 and restarts it. Channels 3 and 4
  loop the descriptor table into SM0 the same way.

Pixels are ordinary little-endian RGB565, so Slint's software renderer can draw straight into
the frame buffer. The 450 KiB buffer can live in either memory:

- **SRAM** (`FrameBuffer::take()`): 63 Hz refresh (16.67 MHz dot clock).
- **PSRAM** (`FrameBuffer::in_psram(&psram)`): 47 Hz refresh (12.5 MHz dot clock). PSRAM
  delivers about 40 MB/s (measured on a Presto), and this uses 25 MB/s of it. Core 1 is
  dedicated to the panel, as in Pimoroni's driver:
  - DMA channel 8 copies the frame into a 64 KiB SRAM ring, running on into the next frame
    so the ring is full when a frame starts.
  - Core 1 moves words from the ring into SM1's FIFO itself. It runs from SRAM with
    interrupts off, so a PSRAM read stuck behind the CPU never stalls the panel.
  - At the end of every frame core 1 resets SM1 and starts again from the first pixel.
  - Code fetched from flash shares the QSPI bus with the copy. When the ring runs below half,
    core 1 rings core 0's doorbell interrupt (`SIO_IRQ_BELL`, taken by the driver), which
    holds core 0 until the copy has caught up (well under a millisecond).
  - Draw with `FrameBuffer::write_span`, which also waits while the ring is running low.
  - `St7701::underruns()`, `stalls()` and `late_frames()` count glitches; `throttles()`
    counts core 0 pauses.

Use PSRAM for the frame buffer whenever you need a heap. The RP2350 only supports atomic
read-modify-write (exclusive load/store) in SRAM: an `Rc`/`Arc`-style counter in PSRAM spins
forever, and Slint is full of them. So keep heaps in SRAM and put the frame buffer in PSRAM.
`St7701::wait_vblank()` resolves at the start of vertical blanking.

The waveforms are checked cycle by cycle in an RP2350 emulator. See [`emu/`](emu/README.md).

## Quick start

```rust
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use presto_bsp::Presto;
use presto_bsp::lcd::{FrameBuffer, Rgb565Pixel};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut board = Presto::default();
    let mut fb = FrameBuffer::take().unwrap();
    fb.pixels_mut().fill(Rgb565Pixel(0x001f)); // blue
    board.lcd.start(fb).unwrap();
    board.backlight.set_percent(80);
    loop {
        board.lcd.wait_vblank().await;
    }
}
```

Every binary needs a global allocator, because Slint (used for the pixel type and by the UI
examples) links `alloc`. See `examples/src/lib.rs`.

## Examples

See [`examples/`](examples/README.md), including the **nursery display** (Slint UI with an
air-quality sensor) and its desktop simulator.

## Cargo features

- `defmt` (default): logging support in this crate and its dependencies.
