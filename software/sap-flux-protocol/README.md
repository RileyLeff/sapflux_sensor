# sap-flux-protocol

`no_std`, dependency-free wire protocol (version 1) for the sap-flux
instrument, shared between the STM32G0 Embassy firmware and host tools such
as [`sap-flux-monitor`](../sap-flux-monitor).

- COBS framing with a trailing `0x00` delimiter
- 8-byte header (`version`, `message_type`, `payload_len` u16 LE,
  `sequence` u32 LE) + payload + IEEE CRC-32 (LE) over all preceding bytes
- Max decoded frame size 128 bytes, all payload sizes fixed per type
- Board→host: Hello `0x10`, Sample `0x11`, Status `0x12`, Ack `0x13`,
  Error `0x14`; host→board: Query `0x01`, SetRtc `0x02`, StartBaseline
  `0x03`, ArmPulse `0x04`, FirePulse `0x05`, Abort `0x06`
- Push-style streaming [`Decoder`] that never panics or allocates and
  resynchronizes at the next delimiter after any corrupt input
- `Sample.flags` bit 5 (`FLAG_COMMAND_WINDOW`) marks a host command window:
  the link is half-duplex RS-485, and after transmitting a Sample with this
  bit set the board stays silent and listening for ≥ ~40 ms. Hosts queue
  commands and transmit at most one immediately after decoding such a
  sample — never at any other time, and never with automatic retries. The
  flag is RS-485-only; a physically full-duplex TTL UART can send commands at
  any time, through the same decoder and board safety state machine
- `Hello.capabilities` bits (`CAP_*`): 0 = RTC, 1 = ADC, 2 = 24 V monitor,
  3 = heater control. Heater-incapable firmware omits bit 3; hosts must
  disable heater controls when it is absent

The full byte-level layout is documented in `src/lib.rs` (also via
`cargo doc --open`) and on each message struct.

```sh
cargo test                                # host tests incl. golden vectors
cargo check --target thumbv6m-none-eabi   # proves no_std on Cortex-M0+
```

Firmware note: `Decoder` is `const`-constructible and ~390 bytes; feed it
UART bytes and match on `push()`. `encode()` writes into a caller-provided
`[u8; MAX_ENCODED_LEN]`.
