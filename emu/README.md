# Display verification in an RP2350 emulator

This crate runs the real `lcd_test_pattern` firmware in
[`rp2350-emu`](https://crates.io/crates/rp2350-emu). Both Cortex-M33 cores, PIO and DMA are
emulated cycle by cycle. The harness acts as a logic analyser on the panel pins:

- It samples every GPIO on every system clock cycle.
- It decodes HSYNC, VSYNC, the dot clock and the 16 data lines the way the ST7701 does in HV
  mode.
- It rebuilds three consecutive frames and compares them pixel by pixel with the pattern the
  firmware drew.

It checks:

- **Dot clock:** period and jitter.
- **Line timing:** dots per line and HSYNC width.
- **Frame timing:** lines per frame and VSYNC width.
- **Pixel position:** pixel 0 lands on the 47th rising edge after HSYNC (the panel's
  HBP = 46).
- **Setup/hold:** at least one PIO cycle around every rising edge.
- **Content:** the full frame matches the pattern.

```
./setup.sh                                                  # once
(cd ../examples && cargo build --release --bin lcd_test_pattern)
cargo run --release
```

Expected output ends with
`PASS: 3 frames match the reference pattern with correct sync timing`. The captured frames are
written to `out/frame<N>.ppm`.

The emulator boots through a minimal stand-in boot ROM, so the RP2350 mask ROM is not needed.

## Emulator fixes

`rp2350-emu` 0.2.6 and `picoem-common` 0.2.2 needed six fixes before they could run this
firmware. `setup.sh` copies both crates from the cargo registry into `vendor/` and applies
`patches/*.patch`. All of these are worth sending upstream:

1. **ARMv8-M load-acquire/store-release** (`LDA*`, `STL*`, `LDAEX*`, `STLEX*`) were not
   decoded. embassy-executor's atomics compile to them.
2. **SPI1 base address** was `0x4008_4000`; on the RP2350 it is `0x4008_8000`.
3. **SPI receive:** in normal (non-loopback) mode a transfer never put a word into the RX FIFO,
   so blocking SPI writes that drain RX hung.
4. **QMI `DIRECT_CSR`** reported TXEMPTY/RXEMPTY on the wrong bits, so PSRAM detection spun
   forever. No PSRAM device is modelled, so the BSP now reports `psram: None`.
5. **DMA trigger reload:** triggering an idle channel through `READ_ADDR_TRIG` did not reload
   `TRANS_COUNT`, which breaks the standard self-restarting DMA loop.
6. **PIO:**
   - A stalled `wait irq` cleared its flag in the stall check and then re-executed the WAIT,
     which stalled forever.
   - `SM_RESTART` reset PC, X, Y and the OSR, which the datasheet says it does not touch.

Both crates' own test suites pass with these changes, plus new regression tests.
`regen-patch.sh` rewrites the patches after editing `vendor/`.

## Not covered

- **PSRAM is not modelled**, so the `nursery` firmware (frame buffer in PSRAM, streamed through the core 1 ring) cannot boot here.
  Its UI is exercised by the desktop simulator, which uses the same renderer.
- **Analogue behaviour:** the emulator is cycle-accurate for logic, not for pad slew rates or
  the panel's own timing tolerances.
