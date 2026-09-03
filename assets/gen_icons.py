#!/usr/bin/env python3
"""Generate TunCat tray icons: white line-art cat on a solid rounded square.

Pure standard library (zlib + struct) — no Pillow required.
Outputs: tray_ok.png / tray_busy.png / tray_err.png / tray_paused.png
"""

import math
import os
import struct
import zlib

SS = 4          # supersampling factor
SIZE = 64       # output canvas
LINE_W = 3.0    # main stroke width
THIN_W = 1.6    # detail stroke width

WHITE = (255, 255, 255, 255)

COLORS = {
    "ok": (47, 164, 106),      # green
    "busy": (232, 163, 61),    # amber
    "err": (210, 84, 70),      # red
    "paused": (138, 143, 152), # gray
}


def clamp(v, lo, hi):
    return max(lo, min(hi, v))


def dist_seg(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    l2 = dx * dx + dy * dy
    if l2 == 0:
        return math.hypot(px - ax, py - ay)
    t = clamp(((px - ax) * dx + (py - ay) * dy) / l2, 0.0, 1.0)
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def dist_circle(px, py, cx, cy, r):
    return abs(math.hypot(px - cx, py - cy) - r)


def dist_dot(px, py, cx, cy, r):
    return math.hypot(px - cx, py - cy) - r


def dist_rrect(px, py, x0, y0, x1, y1, r):
    cx = (x0 + x1) / 2
    cy = (y0 + y1) / 2
    hw = (x1 - x0) / 2 - r
    hh = (y1 - y0) / 2 - r
    qx = abs(px - cx) - hw
    qy = abs(py - cy) - hh
    outside = math.hypot(max(qx, 0), max(qy, 0))
    inside = min(max(qx, qy), 0)
    return outside + inside - r


def coverage(dist, half_width):
    """Anti-aliased coverage from a signed distance (positive = far)."""
    return clamp(0.5 - dist / (half_width * 2) * 2, 0.0, 1.0) if False else clamp(
        (half_width - dist) / (2.0 * half_width) + 0.5, 0.0, 1.0
    )


def build_cat_geometry():
    """Lines: list of (dist_fn, width). Distance > 0 = away from stroke."""
    lines = []

    def seg(ax, ay, bx, by, w=LINE_W):
        lines.append((lambda x, y, a=ax, b=ay, c=bx, d=by, ww=w: dist_seg(x, y, a, b, c, d), w))

    def circle(cx, cy, r, w=LINE_W):
        lines.append((lambda x, y, a=cx, b=cy, rr=r, ww=w: dist_circle(x, y, a, b, rr), w))

    def dot(cx, cy, r):
        lines.append((lambda x, y, a=cx, b=cy, rr=r: dist_dot(x, y, a, b, rr), r * 2))

    # Head.
    circle(32, 37.5, 13.5)

    # Ears: apex to base points on the head circle.
    seg(20, 13, 24.3, 26.4)
    seg(20, 13, 19.0, 34.0)
    seg(44, 13, 39.7, 26.4)
    seg(44, 13, 45.0, 34.0)

    # Eyes.
    dot(26.5, 36.5, 1.7)
    dot(37.5, 36.5, 1.7)

    # Nose + mouth.
    seg(30.2, 40.6, 33.8, 40.6, THIN_W)
    seg(30.2, 40.6, 32, 42.6, THIN_W)
    seg(33.8, 40.6, 32, 42.6, THIN_W)
    seg(32, 42.6, 29.6, 44.2, THIN_W)
    seg(32, 42.6, 34.4, 44.2, THIN_W)

    # Whiskers.
    seg(17.5, 38.5, 9.5, 37.5, THIN_W)
    seg(18.0, 41.5, 10.0, 42.3, THIN_W)
    seg(46.5, 38.5, 54.5, 37.5, THIN_W)
    seg(46.0, 41.5, 54.0, 42.3, THIN_W)

    return lines


CAT = build_cat_geometry()
BG = (3, 3, 61, 61, 13)  # x0, y0, x1, y1, radius


def render(color):
    br, bg_, bb = color
    canvas = [[(0, 0, 0, 0)] * SIZE for _ in range(SIZE)]
    big = SIZE * SS
    for y in range(SIZE):
        for x in range(SIZE):
            r_acc = g_acc = b_acc = a_acc = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    px = (x * SS + sx + 0.5) / SS
                    py = (y * SS + sy + 0.5) / SS

                    x0, y0, x1, y1, rad = BG
                    bg_cov = coverage(dist_rrect(px, py, x0, y0, x1, y1, rad), 0.5)
                    if bg_cov <= 0:
                        continue
                    cr, cg, cb = br, bg_, bb
                    ca = bg_cov

                    line_a = 0.0
                    for fn, w in CAT:
                        d = fn(px, py)
                        line_a = max(line_a, coverage(d, w / 2))
                    if line_a > 0:
                        cr = cr * (1 - line_a) + 255 * line_a
                        cg = cg * (1 - line_a) + 255 * line_a
                        cb = cb * (1 - line_a) + 255 * line_a
                        ca = ca * (1 - line_a * 0.15) + line_a * 0.15

                    r_acc += cr * ca
                    g_acc += cg * ca
                    b_acc += cb * ca
                    a_acc += ca
            n = SS * SS
            if a_acc > 0:
                canvas[y][x] = (
                    int(clamp(r_acc / n, 0, 255)),
                    int(clamp(g_acc / n, 0, 255)),
                    int(clamp(b_acc / n, 0, 255)),
                    int(clamp(a_acc / n * 255, 0, 255)),
                )
    return canvas


def write_png(path, canvas):
    w = h = SIZE
    raw = b""
    for row in canvas:
        raw += b"\x00" + bytes(v for px in row for v in px)

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)
    print(f"wrote {path} ({os.path.getsize(path)} bytes)")


def main():
    out_dir = os.path.dirname(os.path.abspath(__file__))
    for name, color in COLORS.items():
        write_png(os.path.join(out_dir, f"tray_{name}.png"), render(color))


if __name__ == "__main__":
    main()
