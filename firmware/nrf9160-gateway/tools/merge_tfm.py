"""Merge the NCS 2.1.2 TF-M secure image with the Rust non-secure image.

Usage: merge_tfm.py TFM_HEX RUST_NS_HEX OUT_BIN

Writes a flat binary starting at 0x10200 (the MCUboot slot-0 image start,
just after the 0x200 header imgtool adds). Runs under the NCS toolchain's
python3, which ships intelhex.
"""

import sys

from intelhex import IntelHex

IMAGE_START = 0x10200
NS_START = 0x1C200
SLOT_END = 0x10000 + 0x78000  # slot 0 is 0x10000..0x88000 incl. header/trailer


def main() -> int:
    tfm_path, ns_path, out_path = sys.argv[1:4]
    tfm = IntelHex(tfm_path)
    ns = IntelHex(ns_path)
    if tfm.minaddr() != IMAGE_START or tfm.maxaddr() >= NS_START:
        print(f"unexpected TF-M range 0x{tfm.minaddr():x}..0x{tfm.maxaddr():x}", file=sys.stderr)
        return 1
    if ns.minaddr() != NS_START:
        print(f"Rust image must start at 0x{NS_START:x}, got 0x{ns.minaddr():x}", file=sys.stderr)
        return 1
    # The TF-M image carries the reset vector start address; the Rust hex
    # has its own. Keep TF-M's.
    ns.start_addr = None
    tfm.merge(ns, overlap="error")
    end = tfm.maxaddr() + 1
    # Leave room for the MCUboot trailer at the end of the slot.
    if end > SLOT_END - 0x1000:
        print(f"image too large: ends at 0x{end:x}", file=sys.stderr)
        return 1
    tfm.tobinfile(out_path, start=IMAGE_START, end=tfm.maxaddr())
    print(f"merged 0x{IMAGE_START:x}..0x{end:x} ({end - IMAGE_START} bytes) -> {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
