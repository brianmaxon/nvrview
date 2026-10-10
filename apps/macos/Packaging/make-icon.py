"""Draws the placeholder app icon (a generic camera lens on a dark rounded
square) as a PNG using only the standard library. Original artwork, no third
party assets. Usage: make-icon.py SIZE OUT.png"""
import math
import struct
import sys
import zlib


def pixel(x, y, n):
    u, v = (x + 0.5) / n, (y + 0.5) / n
    # Rounded square, 10% margin, corner radius 22% of the icon.
    m, r = 0.1, 0.22
    cx = min(max(u, m + r), 1 - m - r)
    cy = min(max(v, m + r), 1 - m - r)
    if math.hypot(u - cx, v - cy) > r:
        return (0, 0, 0, 0)
    t = v
    base = (int(38 + 20 * t), int(44 + 22 * t), int(58 + 30 * t))
    d = math.hypot(u - 0.5, v - 0.5)
    if d < 0.30:
        k = d / 0.30
        c = (int(20 + 25 * k), int(24 + 30 * k), int(32 + 40 * k))
        if d < 0.17:
            c = (int(15 + 40 * (1 - d / 0.17)), int(30 + 90 * (1 - d / 0.17)), int(60 + 150 * (1 - d / 0.17)))
        if math.hypot(u - 0.44, v - 0.43) < 0.035:
            c = (230, 240, 255)
        return c + (255,)
    return base + (255,)


def main():
    n = int(sys.argv[1])
    rows = []
    ss = 2 if n <= 256 else 1
    for y in range(n):
        row = bytearray([0])
        for x in range(n):
            if ss == 1:
                px = pixel(x, y, n)
            else:
                s = [pixel(x * ss + i, y * ss + j, n * ss) for i in range(ss) for j in range(ss)]
                a = sum(p[3] for p in s)
                px = tuple(sum(p[c] * p[3] for p in s) // max(a, 1) for c in range(3)) + (a // len(s),)
            row += bytes(px)
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0)) \
        + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    with open(sys.argv[2], "wb") as f:
        f.write(png)


main()
