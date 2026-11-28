"""Draws Glint's icons (the four-point spark) without any image library.

    python3 app/icons/make.py      # writes app/icons/*.png and icon.icns (macOS iconutil)

App icon: a graphite rounded square with the lime spark. Tray icons: 22 pt @2x: a black
template spark (macOS tints it), coloured ones for "failed" and "waiting", and a struck-through
template spark for "glintd is not running", so offline never looks like all quiet.
"""
import math, os, shutil, struct, subprocess, sys, zlib

HERE = os.path.dirname(os.path.abspath(__file__))

# The spark from ui/index.html: M12 1.5l1.9 8.6 8.6 1.9-8.6 1.9L12 22.5l-1.9-8.6L1.5 12l8.6-1.9z
SPARK = [(12, 1.5), (13.9, 10.1), (22.5, 12), (13.9, 13.9), (12, 22.5), (10.1, 13.9), (1.5, 12), (10.1, 10.1)]


def inside(poly, x, y):
    c = False
    for i in range(len(poly)):
        (x1, y1), (x2, y2) = poly[i], poly[i - 1]
        if (y1 > y) != (y2 > y) and x < (x2 - x1) * (y - y1) / (y2 - y1) + x1:
            c = not c
    return c


def rounded(size, r, x, y, pad):
    lo, hi = pad + r, size - pad - r
    cx = min(max(x, lo), hi)
    cy = min(max(y, lo), hi)
    return pad <= x <= size - pad and pad <= y <= size - pad and (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def slash_dist(size, x, y):
    """Distance from the top-left to bottom-right diagonal, in pixels."""
    return abs(x - y) / math.sqrt(2) if 0.12 * size <= (x + y) / 2 <= 0.88 * size else 1e9


def render(size, bg=None, fg=(0, 0, 0), spark_scale=1.0, ss=4, slash=0):
    """RGBA rows. bg: (r,g,b) rounded square or None; fg: spark colour; slash: width of a
    diagonal stroke through the spark (with a clear gap on both sides), 0 for none."""
    k = size / 24 * spark_scale
    off = size * (1 - spark_scale) / 2
    poly = [(off + px * k, off + py * k) for px, py in SPARK]
    pad = size * 0.1 if bg else 0  # macOS app icons: 824 of 1024, centred
    radius = (size - 2 * pad) * 0.225
    rows = []
    for j in range(size):
        row = bytearray()
        for i in range(size):
            spark = square = 0
            for sy in range(ss):
                for sx in range(ss):
                    x, y = i + (sx + 0.5) / ss, j + (sy + 0.5) / ss
                    d = slash_dist(size, x, y) if slash else 1e9
                    if d <= slash / 2 or (inside(poly, x, y) and d > slash / 2 + slash * 0.8):
                        spark += 1
                    if bg and rounded(size, radius, x, y, pad):
                        square += 1
            n = ss * ss
            a_s, a_b = spark / n, (square / n if bg else 0)
            if bg:
                a = a_b
                col = [round(bg[c] * (1 - a_s) + fg[c] * a_s) for c in range(3)]
            else:
                a = a_s
                col = list(fg)
            row += bytes(col + [round(a * 255)])
        rows.append(bytes(row))
    return rows


def png(path, rows):
    size = len(rows)
    raw = b"".join(b"\x00" + r for r in rows)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) \
        + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(data)


GRAPHITE = (24, 26, 33)       # ≈ oklch(21% .012 265)
SPARK_LIME = (214, 245, 84)   # ≈ oklch(91% .2 122)
FAIL = (226, 72, 62)          # ≈ oklch(62% .19 25)
WAIT = (232, 160, 40)         # ≈ oklch(76% .15 70)


def main():
    big = render(1024, bg=GRAPHITE, fg=SPARK_LIME, spark_scale=0.5, ss=2)
    png(os.path.join(HERE, "icon.png"), big)
    for s in (32, 128, 256):
        png(os.path.join(HERE, f"{s}x{s}.png"), render(s, bg=GRAPHITE, fg=SPARK_LIME, spark_scale=0.5))
    png(os.path.join(HERE, "128x128@2x.png"), render(256, bg=GRAPHITE, fg=SPARK_LIME, spark_scale=0.5))
    for name, col in (("tray", (0, 0, 0)), ("tray-fail", FAIL), ("tray-wait", WAIT)):
        png(os.path.join(HERE, f"{name}.png"), render(44, fg=col, spark_scale=0.92))
    png(os.path.join(HERE, "tray-offline.png"), render(44, spark_scale=0.92, slash=3.5))

    if sys.platform == "darwin" and shutil.which("iconutil"):
        iconset = os.path.join(HERE, "icon.iconset")
        os.makedirs(iconset, exist_ok=True)
        for s in (16, 32, 128, 256, 512):
            png(os.path.join(iconset, f"icon_{s}x{s}.png"), render(s, bg=GRAPHITE, fg=SPARK_LIME, spark_scale=0.5, ss=2 if s >= 256 else 4))
            png(os.path.join(iconset, f"icon_{s}x{s}@2x.png"), big if s == 512 else render(s * 2, bg=GRAPHITE, fg=SPARK_LIME, spark_scale=0.5, ss=2 if s >= 128 else 4))
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(HERE, "icon.icns")], check=True)
        shutil.rmtree(iconset)


if __name__ == "__main__":
    main()
