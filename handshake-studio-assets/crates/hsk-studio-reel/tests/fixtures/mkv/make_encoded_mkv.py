#!/usr/bin/env python3
"""OFFLINE fixture generator for Matroska ContentEncodings (RFC 9559 section 5.1.4.1.31), run ONCE.

Never run by tests, never a product dependency. It writes EBML byte by byte from the RFC layouts, with
Python's zlib (the reference implementation of RFC 1950/1951) as the independent encoder, and prints the
expected per-packet SHA-256 of the DECODED frames. ffprobe (-show_packets -show_data_hash sha256) is the
second, independent oracle for the single-encoding files.

Usage (artifact root on D:, never C:):
    mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && python make_encoded_mkv.py
"""
import hashlib
import json
import zlib

STRIP = bytes([0x00, 0x00, 0x01, 0xB3])
PRIVATE = b"hsk-reel-codec-private-config-bytes" * 4


def vid(n):  # element id bytes
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b


def vsize(n):
    for length in range(1, 9):
        if n < (1 << (7 * length)) - 1:
            return (n | (1 << (7 * length))).to_bytes(length, "big")
    raise ValueError


def el(i, payload):
    return vid(i) + vsize(len(payload)) + payload


def uint(i, v):
    n = max(1, (v.bit_length() + 7) // 8)
    return el(i, v.to_bytes(n, "big"))


def frame(index, size):
    """Deterministic, compressible, distinct per frame; every frame starts with STRIP."""
    body = bytearray(STRIP)
    for j in range(len(STRIP), size):
        body.append((index * 31 + (j * 7) // 5 + (j >> 6) * 3) & 0xFF)
    return bytes(body[:size])


def encode(kind, original):
    if kind == "zlib":
        return zlib.compress(original, 9)
    if kind == "strip":
        assert original.startswith(STRIP)
        return original[len(STRIP):]
    if kind == "strip+zlib":  # decode order is highest ContentEncodingOrder first: zlib (1), then strip (0)
        return zlib.compress(original[len(STRIP):], 9)
    raise ValueError(kind)


def comp(order, scope, algo, settings=None):
    c = uint(0x4254, algo)
    if settings is not None:
        c += el(0x4255, settings)
    return el(0x6240, uint(0x5031, order) + uint(0x5032, scope) + uint(0x5033, 0) + el(0x5034, c))


def track(encodings, private):
    t = uint(0xD7, 1) + uint(0x73C5, 11) + uint(0x83, 1) + uint(0x9C, 1)
    t += uint(0x23E383, 40_000_000) + el(0x86, b"V_VP8")
    if private is not None:
        t += el(0x63A2, private)
    t += el(0xE0, uint(0xB0, 64) + uint(0xBA, 36))
    t += el(0x6D80, encodings)
    return el(0xAE, t)


def simple_block(rel_ts, key, frames, lacing=0):
    flags = (0x80 if key else 0) | (lacing << 1)
    payload = bytes([0x81]) + rel_ts.to_bytes(2, "big", signed=True) + bytes([flags])
    if lacing:
        payload += bytes([len(frames) - 1])
        for f in frames[:-1]:  # Xiph lacing sizes
            n = len(f)
            while n >= 255:
                payload += b"\xff"
                n -= 255
            payload += bytes([n])
    payload += b"".join(frames)
    return el(0xA3, payload)


def build(kind, encodings, private_stored, plan):
    """plan: list of (rel_ts, key, [original frames]) ; one Xiph-laced block when it has 3 frames."""
    blocks, decoded = b"", []
    for rel_ts, key, originals in plan:
        stored = [encode(kind, o) for o in originals]
        blocks += simple_block(rel_ts, key, stored, lacing=1 if len(stored) > 1 else 0)
        decoded += originals
    cluster = el(0x1F43B675, uint(0xE7, 0) + blocks)
    ebml = el(0x1A45DFA3, uint(0x4286, 1) + uint(0x42F7, 1) + uint(0x42F2, 4) + uint(0x42F3, 8)
              + el(0x4282, b"matroska") + uint(0x4287, 4) + uint(0x4285, 2))
    info = el(0x1549A966, uint(0x2AD7B1, 1_000_000))
    segment = el(0x18538067, info + el(0x1654AE6B, track(encodings, private_stored)) + cluster)
    return ebml + segment, decoded


def plan():
    return [
        (0, True, [frame(0, 3000)]),
        (40, False, [frame(1, 2500)]),
        (80, False, [frame(2, 4096)]),
        (120, False, [frame(3, 4)]),  # exactly the stripped header: stored size 0 under header stripping
        (160, False, [frame(4, 700), frame(5, 1200), frame(6, 50)]),  # one laced block, 3 frames
    ]


files = {
    "enc-zlib.mkv": build("zlib", comp(0, 1, 0, None), None, plan()),
    "enc-headerstrip.mkv": build("strip", comp(0, 1, 3, STRIP), None, plan()),
    "enc-stacked-private.mkv": build(
        "strip+zlib",
        comp(1, 3, 0, None) + comp(0, 1, 3, STRIP),  # zlib on frames AND CodecPrivate; strip on frames
        zlib.compress(PRIVATE, 9),
        plan(),
    ),
}
# Decompression bomb: one 16 MiB frame of zeros compresses to ~16 KB.
bomb_plan = [(0, True, [bytes(STRIP) + bytes(16 * 1024 * 1024 - len(STRIP))])]
files["enc-zlib-bomb.mkv"] = build("zlib", comp(0, 1, 0, None), None, bomb_plan)

manifest = {}
for name, (data, decoded) in files.items():
    with open(name, "wb") as out:
        out.write(data)
    manifest[name] = {
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "frames": len(decoded),
        "decoded_sizes": [len(f) for f in decoded],
        "decoded_sha256": [hashlib.sha256(f).hexdigest() for f in decoded] if len(decoded) < 20 else "see bomb",
    }
manifest["codec_private_original_sha256"] = hashlib.sha256(PRIVATE).hexdigest()
print(json.dumps(manifest, indent=1))
