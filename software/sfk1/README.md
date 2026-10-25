# sfk1

SFK1 is the per-pulse heat-pulse compression format of the sap-flux board.
One record holds a whole heat-pulse window in about 250 bytes, with no
measurable loss of sap-velocity information. For the East30 probe a window
is 60 s of baseline plus 181 s from heater onset, 12 thermistors at 25 Hz,
about 150 kB raw.

The format and the science behind it are in
`analysis/compression_research_20261024/README.md` §5. The reference
implementation is `encoder_spec.py` in the same directory.

* **`Encoder<N, P>`** runs on the board. It is `no_std`, does not allocate,
  has no panic paths, uses no floats and no `unsafe`. You push one frame at a
  time and it writes the record into a caller buffer. `N` is the thermistor
  count and `P` the [`Profile`](#profiles).
* **`Decoder<P>`** (feature `decode`, on by default) is `no_std` too, and
  uses `libm`. It learns the channel count from the record and rebuilds
  each channel's curve.
* **`sfk1::decode`** (feature `alloc`) picks the East30 profile from the
  header and returns owned `Vec`s, serde-ready, for the server.

`Encoder12` and `Encoder16` are the board's encoders: 12 thermistors with
`East30K12` (record version `0x53`, the default) or `East30K16` (`0x54`).
They are byte-exact with the Python reference's firmware mode,
`encoder_spec.encode(w, fixed_point="firmware")`. The decoder matches
`encoder_spec.decode` to 6e-11 counts. Golden vectors in `tests/vectors`
check both.

## Byte layout

A record is a header, then a bitstream with one block per live channel in
ascending channel order. The bitstream is zero-padded to a whole byte.
Multi-byte fields are big-endian, and bits are written MSB first.

### Header

Compact header, 6 bytes. It is used for the East30 profiles with `N = 12`,
so these are the board's records, unchanged:

| bytes | field | |
|---|---|---|
| 0 | version | `0x53` (East30, 12 knots) or `0x54` (16 knots) |
| 1-2 | live mask | `u16`, bit `c-1` = thermistor `c`; bits 12-15 are 0 |
| 3-4 | `E_ref` | `u16`, reference excitation in ADC counts |
| 5 | `NB` | baseline bins before onset (2-12 on the board) |

Extended header, `10 + ceil(N/8)` bytes. It is used for every other channel
count or profile:

| bytes | field | |
|---|---|---|
| 0 | version | `0x5F` |
| 1-4 | profile id | `u32`, `Profile::ID` (see below) |
| 5-6 | `N` | `u16`, the sensor's channel count, 1-65535 |
| 7 .. 7+M | live mask | `M = ceil(N/8)` bytes, one big-endian integer, bit `c-1` = channel `c`; bits `>= N` are 0 |
| 7+M .. 9+M | `E_ref` | `u16` |
| 9+M | `NB` | baseline bins |

A decoder refuses a record whose version byte, or profile id, is not its
profile's (`DecodeError::WrongProfile`). The East30 ids are `0xc6c86f9c`
(K12) and `0x1cbba4f7` (K16). A 1-channel East30 record therefore has an
11-byte extended header, and a 20-channel one has a 13-byte header.

### Per-channel block

East30 widths are in parentheses; all of them are profile constants.

| field | bits | meaning |
|---|---|---|
| `b0` | `B0_BITS` (19) | first baseline bin, unsigned, 1/8 counts at `E_ref` |
| `kb` | 4 | Rice parameter of the bin deltas |
| `d_1 .. d_{NB-1}` | Rice(`kb`) | `b_i - b_{i-1}` |
| `o` | `OFFSET_BITS` (2) | Rice offset, stored as `o - OFFSET_MIN` (East30: `o` in -1..=2) |
| `ci_0 .. ci_{K+3}` | Rice(`max(0, K_TABLE[j] + o)`) | quantized spline coefficients, `STEP` (3) counts each |

`Rice_k(v)` writes `u = zigzag(v)` (0, -1, 1, -2 map to 0, 1, 2, 3) as
`u >> k` ones, a zero, then the low `k` bits of `u`. If `u >> k` is at least
`ESCAPE_Q` (24), it writes `ESCAPE_Q` ones followed by `u` in `ESCAPE_BITS`
(20) bits instead. The encoder clamps coefficients to that range.

### Decoding

The decoded curve is in counts at `E_ref`, with `t` in seconds from onset.

The baseline is the least-squares line through the bin centres:

```
((BF·i + (BF-1)/2 − BF·NB) / fs,  b_i / 8)
```

Before onset, the curve is that line plus the bin residuals, interpolated
linearly between bin centres.

From onset, the curve is the line plus `Σ_j B_j(t) w_j`, where:

* `w = D · (STEP · ci)`;
* `B_j` are the cubic B-splines in `ln(t + t0)`, with uniform interior knots
  over `[ln t0, ln(t_end + t0) + 1e-9]`.

## Profiles

A `Profile` is a unit struct that describes a sensor shape and its trained
tables:

* the grid: sample rate, bin length, post-onset length, the log-time offset
  `t0`;
* the basis: knot count;
* the matrices: the encoder matrix `MQ` (i32) and the decoder matrix `D`
  (f64);
* the entropy coder: Rice k-table, offset field and escape.

The channel count is not part of the profile; it is the encoder's `N`.
Every item except the tables and `KNOTS` has an East30 default.

`Profile::ID` defaults to `fingerprint::<P>()`. That is an FNV-1a hash of
every constant that affects the bitstream or its meaning, so changing any
table changes the id. Encoding a profile with an invariant broken (say, the
`Acc` array not `KNOTS + 4` long) fails to compile, and the error names the
rule. `Profile::validate()` makes the same check at run time.

To make a new profile, generate its tables from the same physics training as
East30:

```sh
cd analysis/compression_research_20261024
uv run --with numpy --with scipy --with pandas --with matplotlib \
    python ../../software/sfk1/tools/generate.py \
    --profile probe48_10hz --period-us 100000 --post-s 181 --t0 2 --knots 12
```

This writes `tests/profiles/probe48_10hz.rs`. Include it and implement the
trait; the crate docs and `tests/custom_profile.rs` have the full example (a
48-thermistor probe at 10 Hz, 28 B per channel):

```rust
struct Probe48;
impl sfk1::Profile for Probe48 {
    const NAME: &'static str = "probe48 10 Hz";
    const FRAME_PERIOD_US: u32 = 100_000; // 10 Hz
    const BIN_FRAMES: u16 = 50;
    type Bins = [i32; 12];
    const POST_FRAMES: u16 = 1811;
    const T0_FRAMES: u32 = 20;
    const KNOTS: u8 = 12;
    type Acc = [i64; 16];
    const KF_Q28: i64 = probe48::KF_Q28;
    const MQ: &'static [i32] = probe48::MQ.as_flattened();
    const D: &'static [f64] = probe48::D.as_flattened();
    const K_TABLE: &'static [u8] = &probe48::K_TABLE;
}
let enc = sfk1::Encoder::<48, Probe48>::new(&[0xff; 6], 12)?;
```

**Several excitation groups.** A probe whose thermistors sit on separate
excitation lines is encoded as one encoder per group, each fed its own
excitation, and produces one record per group. Ratiometry stays exact, and
no change to the format is needed.

## On the board (Embassy sketch)

```rust,ignore
use sfk1::{Encoder12, MAX_RECORD_LEN};
use static_cell::StaticCell;

static ENC: StaticCell<Encoder12> = StaticCell::new();
static OUT: StaticCell<[u8; MAX_RECORD_LEN]> = StaticCell::new();

#[embassy_executor::task]
async fn pulse(mut adc: Adc, mut heater: Heater, live: u16, mut log: Flash) {
    let enc = ENC.init(Encoder12::new(&live.to_be_bytes(), 12).unwrap());
    let out = OUT.init([0; MAX_RECORD_LEN]);
    loop {
        enc.reset();
        let mut ticker = Ticker::every(Duration::from_hz(25));
        for n in 0..enc.frames_expected() {
            ticker.next().await;
            if n == enc.onset_frame() {
                heater.fire();
            }
            match adc.frame().await {
                Ok((exc, therm)) => enc.push(exc, &therm)?,  // therm: [i16; 12]
                Err(_) => enc.push_missing()?,               // a dropped conversion
            }
        }
        let done = enc.finish(out)?;
        defmt::info!("pulse: {} B, {} missing, E_ref {}", done.len, done.missing, done.e_ref);
        log.append(&out[..done.len]).await?;
    }
}
```

Build the firmware with `sfk1 = { default-features = false }`, plus
`features = ["defmt"]` if wanted. That links only the integer encoder and
the tables of the profile in use.

## On the server (axum sketch)

```rust,ignore
// sfk1 = { features = ["std", "serde"] }
use axum::{body::Bytes, http::StatusCode, Json};

async fn ingest(body: Bytes) -> Result<Json<sfk1::Decoded>, (StatusCode, String)> {
    // compact or extended header, any channel count: N comes from the record
    sfk1::decode(&body).map(Json).map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}
```

`Decoded` carries:

* `layout`, `profile`, `n_channels`, `e_ref`, `n_bins`, `frame_period_us`
  and `onset_frame`;
* one `DecodedChannel { channel, bins, coefs, samples }` per live channel;
* `times()`, which gives each sample's time in seconds from onset.

For a custom profile, or to avoid allocating, use
`Decoder::<P>::new(record)?` and then `.channels()`.
`ChannelCoefs::samples()` is an iterator, and `reconstruct(&mut buf)` fills
a slice.

## Sizes

Encoder RAM, `size_of`:

| encoder | bytes |
|---|---:|
| `Encoder<1, East30K12>` | 288 |
| `Encoder12` | 2 744 |
| `Encoder16` | 3 128 |
| `Encoder<64, East30K12>` | 14 336 |
| `Encoder<256, East30K12>` | 57 152 |

That is about 220 B per channel. A few hundred channels are fine on a host,
but too much RAM for the STM32G0B1 next to the firmware; split large arrays
into groups.

Flash on thumbv6m (Cortex-M0+), measured as an LTO probe binary minus an
empty one. The figures include the compiler-builtins that the encoder pulls
in (64-bit divide, memcpy), about 1.9 kB, which a firmware may already
link. There are no panic paths in the binary.

| `opt-level` | `Encoder12` .text + .rodata | `Encoder16` |
|---|---:|---:|
| `"s"` | 9.8 kB + 1.2 kB | 9.8 kB + 1.8 kB |
| `"z"` | 10.2 kB + 1.2 kB | 10.1 kB + 1.8 kB |
| `3` | 17.0 kB + 1.1 kB | 15.7 kB + 1.7 kB |

At `opt-level = 3`, LLVM unrolls the per-channel loops. A firmware built
that way can give just this crate `opt-level = "s"`:

```toml
[profile.release.package.sfk1]
opt-level = "s"
```

Records are about 20 B per live channel plus the header: 78-287 B for the
vector windows. `MAX_RECORD_LEN` (2090 B) bounds any `Encoder12` or
`Encoder16` record. Every encoder has its own `Encoder::MAX_RECORD_LEN`.

## Features

| feature | default | what |
|---|---|---|
| `decode` | yes | `Decoder`, `Header`, `ChannelCoefs`; pulls in `libm` |
| `alloc` | | `decode()`, `Decoded` |
| `std` | | implies `alloc` |
| `defmt` | | `defmt::Format` on public types and errors |
| `serde` | | `Serialize`/`Deserialize` on `Layout`, `Line`, `Decoded` |

The MSRV is 1.87.

## Tests and vectors

```sh
cargo test                        # default features
cargo test --no-default-features  # encoder only
cargo test --all-features
cargo build --target thumbv6m-none-eabi --no-default-features
cargo run --example real_pulse --features std   # encode a bench capture
```

* `tests/vectors.rs` re-encodes 5 windows (3 real captures with missing
  frames, 2 synthetic) with K12 and K16 and requires byte-identical records.
  It also compares the decoder with the Python decoder.
* `tests/robustness.rs` checks that nothing panics on noise, rails, dead
  excitation, missing frames, arbitrary bytes or mutated records. It also
  round-trips `N` = 1, 12, 17 and 64, checking that each channel's integers
  and curve equal those of encoding that channel alone, and checks the
  extended header byte for byte.

`tools/generate.py` regenerates `src/tables.rs`, the vectors and custom
profile tables from the Python reference, and checks Rust-made records
against it (`--check-record`).

## License

MIT OR Apache-2.0.
