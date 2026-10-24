#!/bin/sh
# Build static (musl) Linux binaries and pack a deploy bundle.
#   deploy/build.sh            # x86_64 (Hetzner CX/CPX)
#   deploy/build.sh aarch64    # Arm (Hetzner CAX)
# Needs: rustup target add <arch>-unknown-linux-musl, and uv (runs
# cargo-zigbuild + zig ephemerally, nothing installed globally).
# Without uv: `cargo install cargo-zigbuild` + zig, or build on the VPS
# itself with plain `cargo build --release` (glibc binary, also fine).
set -eu
ARCH="${1:-x86_64}"
TARGET="$ARCH-unknown-linux-musl"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$HERE")"
cd "$ROOT"
rustup target add "$TARGET" >/dev/null
uvx --from cargo-zigbuild --with ziglang cargo-zigbuild build --release --target "$TARGET" -p sapflux-server

OUT="$HERE/dist/sapflux-$ARCH"
rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/systemd"
for b in sapflux-server sapflux sapflux-gateway-sim; do
    cp "target/$TARGET/release/$b" "$OUT/bin/"
done
cp "$HERE"/systemd/* "$OUT/systemd/"
cp "$HERE/install.sh" "$HERE/replicate.sh" "$HERE/replicate.env.example" \
   "$HERE/sapflux-cli-wrapper.sh" "$HERE/README.md" "$ROOT/config.example.toml" "$OUT/"
tar -C "$HERE/dist" -czf "$HERE/dist/sapflux-$ARCH.tar.gz" "sapflux-$ARCH"
file "$OUT/bin/sapflux-server"
echo "bundle: $HERE/dist/sapflux-$ARCH.tar.gz"
