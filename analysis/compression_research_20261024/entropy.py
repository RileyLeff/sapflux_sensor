"""Integer entropy coders sized for a Cortex-M0+.

All functions return exact bit counts for the stated bitstream format (Rice
and adaptive Golomb are implemented bit-exactly in `BitWriter`/`BitReader`
for the recommended encoder; the arithmetic/ANS numbers are ideal code
lengths, which real coders reach within a few bytes per stream).
"""

from __future__ import annotations

import math

import numpy as np

RICE_ESCAPE_Q = 24  # unary quotient >= this -> escape + raw 20-bit value


def zigzag(v: np.ndarray) -> np.ndarray:
    v = np.asarray(v, np.int64)
    return np.where(v >= 0, 2 * v, -2 * v - 1)


def unzigzag(u: np.ndarray) -> np.ndarray:
    u = np.asarray(u, np.int64)
    return np.where(u & 1, -(u + 1) // 2, u // 2)


def rice_len(u: np.ndarray, k: int) -> int:
    """Bits for zigzagged values u with Rice parameter k (with escape)."""
    q = u >> k
    esc = q >= RICE_ESCAPE_Q
    return int(np.sum(np.where(esc, RICE_ESCAPE_Q + 20, q + 1 + k)))


def rice_block_bits(v: np.ndarray, block: int = 32, kbits: int = 4) -> int:
    """Block-adaptive Rice: best k per block of `block` values, kbits header."""
    u = zigzag(v)
    total = 0
    for i in range(0, len(u), block):
        b = u[i:i + block]
        total += kbits + min(rice_len(b, k) for k in range(0, 16))
    return total


def rice_stream_bits(v: np.ndarray, kbits: int = 4) -> int:
    """One k for the whole stream."""
    u = zigzag(v)
    if len(u) == 0:
        return 0
    return kbits + min(rice_len(u, k) for k in range(0, 18))


def adaptive_golomb_bits(v: np.ndarray, init_mean: float = 4.0, reset: int = 32) -> int:
    """LOCO-I style adaptive Rice: k from running mean of |u| (no side info).

    k = smallest k with N*2^k >= A; A, N halved when N reaches `reset`."""
    u = zigzag(v)
    a = init_mean
    n = 1
    total = 0
    for x in u:
        k = 0
        while (n << k) < a and k < 20:
            k += 1
        q = int(x) >> k
        total += (RICE_ESCAPE_Q + 20) if q >= RICE_ESCAPE_Q else q + 1 + k
        a += int(x)
        n += 1
        if n >= reset:
            a /= 2
            n //= 2
    return total


def entropy_bits(v: np.ndarray) -> float:
    """Zeroth-order empirical entropy (ideal static coder, table free)."""
    v = np.asarray(v)
    if len(v) == 0:
        return 0.0
    _, c = np.unique(v, return_counts=True)
    p = c / c.sum()
    return float(-np.sum(c * np.log2(p)))


def adaptive_ac_bits(v: np.ndarray, alphabet: int = 64) -> float:
    """Ideal code length of an adaptive arithmetic coder over zigzagged
    values: symbols 0..alphabet-2 directly, alphabet-1 = escape + Elias-gamma.
    Laplace (count+1) initialisation, counts halved at 1024."""
    u = zigzag(v)
    counts = np.ones(alphabet)
    total = 0.0
    for x in u:
        s = min(int(x), alphabet - 1)
        total += -math.log2(counts[s] / counts.sum())
        if s == alphabet - 1:
            r = int(x) - (alphabet - 1) + 1
            total += 2 * int(math.log2(r)) + 1
        counts[s] += 1
        if counts.sum() > 1024:
            counts = np.maximum(1, counts // 2)
    return total


def static_model_bits(v: np.ndarray, table: np.ndarray) -> float:
    """Cross-entropy under a fixed (trained, shared) probability table over
    zigzagged symbols; last entry is escape (+ Elias-gamma).  This is what a
    static tANS/rANS with a firmware-resident table achieves."""
    u = zigzag(v)
    a = len(table)
    s = np.minimum(u, a - 1)
    bits = -np.log2(table[s])
    esc = u >= a - 1
    r = u[esc] - (a - 1) + 1
    return float(bits.sum() + np.sum(2 * np.floor(np.log2(r)) + 1))


def train_table(samples: list[np.ndarray], alphabet: int = 64) -> np.ndarray:
    u = np.concatenate([np.minimum(zigzag(s), alphabet - 1) for s in samples])
    c = np.bincount(u, minlength=alphabet).astype(float) + 0.5
    return c / c.sum()


# --------------------------------------------------------------------------
# Bit-exact writer/reader (used by the reference encoder in encoder_spec.py)


class BitWriter:
    def __init__(self) -> None:
        self.out = bytearray()
        self.acc = 0
        self.n = 0

    def put(self, value: int, bits: int) -> None:
        for i in range(bits - 1, -1, -1):
            self.acc = (self.acc << 1) | ((value >> i) & 1)
            self.n += 1
            if self.n == 8:
                self.out.append(self.acc)
                self.acc = 0
                self.n = 0

    def unary(self, q: int) -> None:
        for _ in range(q):
            self.put(1, 1)
        self.put(0, 1)

    def rice(self, s: int, k: int) -> None:
        u = 2 * s if s >= 0 else -2 * s - 1
        q = u >> k
        if q >= RICE_ESCAPE_Q:
            for _ in range(RICE_ESCAPE_Q):  # escape: Q ones, no terminator
                self.put(1, 1)
            self.put(u, 20)
        else:
            self.unary(q)
            self.put(u & ((1 << k) - 1), k)

    def bytes(self) -> bytes:
        if self.n:
            return bytes(self.out) + bytes([self.acc << (8 - self.n)])
        return bytes(self.out)


class BitReader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    def get(self, bits: int) -> int:
        v = 0
        for _ in range(bits):
            byte = self.data[self.pos >> 3]
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1)
            self.pos += 1
        return v

    def unary(self, cap: int) -> int:
        q = 0
        while q < cap and self.get(1):
            q += 1
        return q

    def rice(self, k: int) -> int:
        q = self.unary(RICE_ESCAPE_Q)
        u = self.get(20) if q >= RICE_ESCAPE_Q else (q << k) | self.get(k)
        return u // 2 if u % 2 == 0 else -(u + 1) // 2
