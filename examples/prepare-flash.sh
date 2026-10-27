#!/bin/sh
# Stop the display's DMA and PIO through the debug probe before flashing.
#
# With the frame buffer in PSRAM, the scan-out keeps reading PSRAM over the QSPI bus while the
# probe halts the CPU. Resetting the chip in the middle of such a transfer leaves the PSRAM
# driving the bus (its chip select floats low after reset), which garbles flash reads until the
# board is power-cycled. Stopping the transfers first avoids that.
set -eu
chip=RP235x
probe-rs write --chip $chip b32 0x50300000 0 >/dev/null 2>&1 || true        # PIO1 CTRL: stop SMs
for _ in 1 2; do
    probe-rs write --chip $chip b32 0x50000464 0xffff >/dev/null 2>&1 || true # DMA CHAN_ABORT
done
