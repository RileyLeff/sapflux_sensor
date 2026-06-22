# SAP Flux Blinky

Minimal Embassy firmware for the SAP Flux Adapter board.

The onboard RGB LED is common-anode. The STM32 sinks the LED cathodes, so the GPIO logic is inverted:

- `PB3` low: red on
- `PB4` low: green on
- `PB5` low: blue on

On the first assembled boards, the LED appears to be rotated 180 degrees:

- `PB5` / board blue net behaves as the accidental common anode
- `PB4` / board green net lights the red die
- `PB3` / board red net lights the blue die
- green is not firmware-recoverable because its cathode appears to land on `V3V3`

The current firmware uses that rotated-board mapping for a clean red/blue bring-up blink.

Build:

```sh
RUSTC=/Users/rileyleff/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc RUSTC_WRAPPER= rustup run stable cargo build --release
```

Flash through the ST-Link:

```sh
RUSTC=/Users/rileyleff/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc RUSTC_WRAPPER= rustup run stable cargo run --release
```

Reset the MCU after flashing:

```sh
probe-rs reset --chip STM32G0B1CB --protocol swd
```
