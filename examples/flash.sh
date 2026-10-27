#!/bin/sh
# Build an example and put it on the Presto.
#
#   ./flash.sh            # the nursery display
#   ./flash.sh touch      # any other example
#
# With a debug probe on the SWD pins: flashes and streams the defmt log (probe-rs).
# Otherwise over USB: hold the Presto's BOOT/user button while plugging in USB (or while pressing
# RESET). The RP2350 shows up as a drive named RP2350 and the firmware is copied onto it; the
# board restarts into it by itself.
set -eu
cd "$(dirname "$0")"
bin="${1:-nursery}"
cargo build --release --bin "$bin"
elf="target/thumbv8m.main-none-eabihf/release/$bin"

if probe-rs list 2>/dev/null | grep -qv "No debug probes"; then
    ./prepare-flash.sh
    exec probe-rs run --chip RP235x "$elf"
fi

if [ ! -d /Volumes/RP2350 ]; then
    echo "No debug probe found. Hold BOOT on the Presto and connect USB (or press RESET while holding BOOT)..."
    i=0
    while [ ! -d /Volumes/RP2350 ]; do
        sleep 1
        i=$((i + 1))
        if [ "$i" -ge 120 ]; then
            echo "Timed out waiting for the RP2350 drive." >&2
            exit 1
        fi
    done
fi
exec elf2uf2-rs deploy --family rp2350-arm-s "$elf"
