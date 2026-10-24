#!/bin/sh
# Build the gateway and produce the signed MCUboot serial-recovery image
# build/app_update.bin (TF-M secure image + Rust non-secure app).
#
#   tools/package.sh [extra cargo args, e.g. --features log-always-on]
#
# Inputs (override with environment variables):
#   TFM_HEX   NCS 2.1.2 TF-M image (tfm_s.hex, 0x10200..0x1c200)
#   NCS       NCS v2.1.2 install (MCUboot root-ec-p256.pem key)
#   NCS_TOOLS NCS v2.1.2 toolchain bin dir (python3 with intelhex, imgtool)
#   OBJCOPY   arm-none-eabi-objcopy
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"

TFM_HEX=${TFM_HEX:-build/modem-dfu-server-ncs212/tfm/bin/tfm_s.hex}
NCS=${NCS:-/opt/nordic/ncs/v2.1.2}
NCS_TOOLS=${NCS_TOOLS:-/opt/nordic/ncs/toolchains/v2.1.2/bin}
OBJCOPY=${OBJCOPY:-arm-none-eabi-objcopy}
KEY="$NCS/bootloader/mcuboot/root-ec-p256.pem"

missing=0
for f in "$TFM_HEX" "$KEY" "$NCS_TOOLS/python3" "$NCS_TOOLS/imgtool"; do
    if [ ! -e "$f" ]; then
        echo "missing: $f" >&2
        missing=1
    fi
done
command -v "$OBJCOPY" >/dev/null 2>&1 || { echo "missing: $OBJCOPY" >&2; missing=1; }
[ "$missing" -eq 0 ] || exit 1

if [ ! -f gateway.local.toml ] && [ -z "${SAPFLUX_GATEWAY_CONFIG:-}" ]; then
    echo "WARNING: no gateway.local.toml: packaging the EXAMPLE config (not deployable)" >&2
fi

cargo build --release "$@"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
elf=target/thumbv8m.main-none-eabihf/release/sapflux-nrf9160-gateway
mkdir -p build
"$OBJCOPY" -O ihex "$elf" build/sapflux-gateway-ns.hex
"$NCS_TOOLS/python3" -I tools/merge_tfm.py "$TFM_HEX" build/sapflux-gateway-ns.hex build/sapflux-gateway-merged.bin
"$NCS_TOOLS/imgtool" sign \
    --key "$KEY" \
    --header-size 0x200 \
    --align 4 \
    --version "$version" \
    --pad-header \
    --slot-size 0x78000 \
    build/sapflux-gateway-merged.bin build/app_update.bin
cp build/app_update.bin "build/sapflux-gateway-$version-app_update.bin"
"$NCS_TOOLS/imgtool" verify --key "$KEY" build/app_update.bin
echo "signed: build/app_update.bin (also build/sapflux-gateway-$version-app_update.bin)"
