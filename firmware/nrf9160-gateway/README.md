# Sapflux nRF9160 gateway

Rust/Embassy firmware for the CircuitDojo nRF9160 Feather used as the cellular
gateway for the sap-flux logic board.

The diagnostic target deliberately uses the Feather's factory-era (2021)
MCUboot serial recovery layout. A TF-M secure image configures the nRF9160 and
launches the Rust/Embassy application in non-secure mode, as required by
Nordic's modem library. The Rust application reports reset cause, modem
firmware, powered UICC state, redacted SIM identity, LTE-M registration,
signal, operator, and PDP context through the onboard CP2102N at 1 Mbaud. The
current long-run diagnostic waits indefinitely for registration while using
the Hologram APN and LTE-M only. It deliberately performs no DNS lookup,
socket operation, HTTP request, or application data transfer.

The modem was upgraded over Nordic's full-modem UART DFU path and verified at
`mfw_nrf9160_1.3.7` during bring-up.

## Hardware map used by bring-up

- Blue LED D7: P0.03, active low
- USB UART RX: P0.05
- USB UART TX: P0.06
- MODE button: P0.12, active low (owned by MCUboot)
- Future SapFlux TTL RX: P0.23 / Feather RX
- Future SapFlux TTL TX: P0.24 / Feather TX

## Build

```sh
cargo build --release
```

The Rust ELF is linked as the non-secure application at `0x0001_c200`. The
known-working NCS 2.1.2 TF-M image occupies `0x0001_0200..0x0001_c200`. Merge
the TF-M and Rust Intel HEX files, convert the merged image to a binary, and
sign it with the Feather bootloader's MCUboot key and a `0x200` header before
serial recovery will accept it. The generated, signed recovery artifact is
`build/app_update.bin`.

Do not flash the raw Rust ELF, Rust HEX, or unsigned merged binary through
MCUboot.
