#!/bin/sh
# Prepare vendor/: rp2350-emu 0.2.6 and picoem-common 0.2.2 from crates.io with the fixes in
# patches/ applied (see the README for what they fix).
set -eu
cd "$(dirname "$0")"
[ -d vendor/rp2350-emu ] && [ -d vendor/picoem-common ] && exit 0
# Make sure the pristine crates are in the cargo registry.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/src" "$tmp/.cargo"
printf '[package]\nname = "fetch"\nversion = "0.0.0"\nedition = "2024"\n[dependencies]\nrp2350-emu = "=0.2.6"\npicoem-common = "=0.2.2"\n' > "$tmp/Cargo.toml"
printf '[build]\ntarget = "host-tuple"\n' > "$tmp/.cargo/config.toml"
: > "$tmp/src/lib.rs"
(cd "$tmp" && cargo fetch -q)
rm -rf vendor
mkdir -p vendor
for crate in rp2350-emu-0.2.6 picoem-common-0.2.2; do
    name=${crate%-*}
    src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"$crate" | head -n 1)
    cp -R "$src" "vendor/$name"
    chmod -R u+w "vendor/$name"
    patch -s -p1 -d "vendor/$name" < "patches/$name.patch"
done
echo "vendor/ ready"
