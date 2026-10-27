#!/bin/sh
# Regenerate the emulator patches from the edited copies in vendor/ (maintainer helper).
set -eu
cd "$(dirname "$0")"
for crate in rp2350-emu-0.2.6 picoem-common-0.2.2; do
    name=${crate%-*}
    src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"$crate" | head -n 1)
    tmp=$(mktemp -d)
    cp -R "$src" "$tmp/a"
    cp -R "vendor/$name" "$tmp/b"
    rm -rf "$tmp/b/target"
    (cd "$tmp" && diff -ruN a b | sed -E 's/^(---|\+\+\+) ([^	]*)	.*/\1 \2/') > "patches/$name.patch" || true
    rm -rf "$tmp"
    echo "$name: $(grep -c '^@@' "patches/$name.patch") hunks"
done
