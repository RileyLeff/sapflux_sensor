# SAP Flux Blinky

Minimal Embassy firmware for the SAP Flux Adapter board.

The onboard RGB LED is common-anode. The STM32 sinks the LED cathodes, so the GPIO logic is inverted:

- `PB3` low: red on
- `PB4` low: green on
- `PB5` low: blue on

On the v0.1 assembled boards, the LED appears to be rotated 180 degrees:

- `PB5` / board blue net behaves as the accidental common anode
- `PB4` / board green net lights the red die
- `PB3` / board red net lights the blue die
- green is not firmware-recoverable because its cathode appears to land on `V3V3`

That note is historical: v0.2 has the corrected component orientations.

## Heater monitor transports

`heater_monitor` always exposes protocol v1 over the isolated RS-485 port at
115200 8N1. It can additionally expose the same protocol on J6 via LPUART2:

- J6 pin 2 / PA13: SapFlux RX (connect to Feather TX through 1 kΩ)
- J6 pin 3 / PA14: SapFlux TX (connect to Feather RX through 1 kΩ)
- J6 pin 4: common ground
- do not connect J6 pin 1 / 3.3 V between the boards

The mode is sampled once at boot from J4 pin 5 / PA6. Leave PA6 open for SWD,
or jumper PA6 to either J4 ground pin before reset to select LPUART2. There is
no hot switching. Before attaching ST-Link, return to SWD mode and make sure
the Feather TX pin is high-impedance or physically disconnected.

UART and RS-485 use the same COBS/CRC decoder and heater safety state machine.
Samples and periodic status are sent on both configured transports; command
ACK/Error traffic returns only on the transport that received the command.
`FLAG_COMMAND_WINDOW` is only a half-duplex RS-485 rule. Full-duplex UART
commands may arrive at any time, but gain no additional heater authority.

In UART mode the board sends Hello at boot and every 30 seconds. Status is
currently sent every second (meeting the field gateway's <=10 second idle and
1 second active liveness requirements). `SetRtc` is accepted only in SafeIdle.

Build the field firmware:

```sh
cargo build --release --bin heater_monitor
```

The supervised bench build retains the intentionally short 30-second
persistent cooldown and advertises firmware version 0.1.32:

```sh
cargo build --release --bin heater_monitor --features dev-short-cooldown
```

Flash through the ST-Link:

```sh
cargo run --release --bin heater_monitor
```

Reset the MCU after flashing:

```sh
probe-rs reset --chip STM32G0B1CB --protocol swd
```
