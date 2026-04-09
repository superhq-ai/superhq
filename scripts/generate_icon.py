#!/usr/bin/env python3
"""Generate AppIcon.icns from pixel art data. No external dependencies."""

import struct
import zlib
import os
import shutil
import subprocess

# 20x20 pixel grid: lowercase "hq" in bold pixel art with padding
# 1 = lit face (left/top), 2 = shadow face (right/bottom), 0 = background
GRID = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 0, 1, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 2, 0, 1, 2, 0, 1, 2, 0, 0, 1, 2, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 0, 0, 1, 2, 0, 1, 2, 0, 0, 1, 2, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 0, 0, 1, 2, 0, 1, 2, 0, 0, 1, 2, 0, 0, 0],
    [0, 0, 0, 0, 1, 2, 0, 0, 1, 2, 0, 0, 2, 2, 2, 1, 2, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
]
GRID_SIZE = 20

BG = (30, 30, 38)  # #1e1e26

# Chrome gradient stops — vertical specular sweep (matches SVG)
# Gradient spans SVG y=240..848, mapped to pixel coordinates per render size.
# Format: (t, (r, g, b))
LIT_STOPS = [
    (0.00, (154, 152, 148)),  # #9a9894
    (0.35, (224, 222, 216)),  # #e0ded8
    (0.50, (236, 234, 228)),  # #eceae4
    (0.65, (224, 222, 216)),  # #e0ded8
    (1.00, (112, 110, 104)),  # #706e68
]

SHD_STOPS = [
    (0.00, (106, 104, 100)),  # #6a6864
    (0.35, (168, 166, 160)),  # #a8a6a0
    (0.50, (184, 182, 176)),  # #b8b6b0
    (0.65, (168, 166, 160)),  # #a8a6a0
    (1.00, (72,  70,  68)),   # #484644
]

# macOS squircle corner radius ≈ 22.37% of icon size
CORNER_RATIO = 0.2237


def lerp_gradient(stops, t):
    """Interpolate a multi-stop gradient at position t (0.0–1.0)."""
    t = max(0.0, min(1.0, t))
    for i in range(len(stops) - 1):
        t0, c0 = stops[i]
        t1, c1 = stops[i + 1]
        if t <= t1:
            f = (t - t0) / (t1 - t0) if t1 > t0 else 0.0
            return (
                int(c0[0] + (c1[0] - c0[0]) * f),
                int(c0[1] + (c1[1] - c0[1]) * f),
                int(c0[2] + (c1[2] - c0[2]) * f),
            )
    return stops[-1][1]


def pixel_color(grid_val, py, size):
    """Get color for a pixel based on grid value and y position."""
    if grid_val == 0:
        return BG
    # Map pixel y to SVG coordinate space (1024-unit viewBox)
    svg_y = (py + 0.5) / size * 1024.0
    # Gradient spans SVG y=240..848
    t = (svg_y - 240.0) / (848.0 - 240.0)
    return lerp_gradient(LIT_STOPS if grid_val == 1 else SHD_STOPS, t)


def make_png(width, height, pixels_rgba):
    """Create a PNG file from raw RGBA pixel data (no dependencies)."""
    def chunk(chunk_type, data):
        c = chunk_type + data
        crc = struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
        return struct.pack(">I", len(data)) + c + crc

    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)

    raw = b""
    for y in range(height):
        raw += b"\x00"
        off = y * width * 4
        raw += pixels_rgba[off : off + width * 4]

    return sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def in_squircle(px, py, size):
    """Check if pixel (px, py) is inside a macOS-style rounded rect."""
    r = size * CORNER_RATIO
    if px >= r and px <= size - r:
        return True
    if py >= r and py <= size - r:
        return True
    cx = r if px < r else size - r
    cy = r if py < r else size - r
    dx = px - cx
    dy = py - cy
    return dx * dx + dy * dy <= r * r


def render(size):
    """Render the grid scaled up to `size` x `size` with chrome gradients."""
    scale = size / GRID_SIZE
    buf = bytearray(size * size * 4)

    for py in range(size):
        for px in range(size):
            idx = (py * size + px) * 4
            if not in_squircle(px + 0.5, py + 0.5, size):
                buf[idx : idx + 4] = b"\x00\x00\x00\x00"
                continue

            gx = min(int(px / scale), GRID_SIZE - 1)
            gy = min(int(py / scale), GRID_SIZE - 1)
            r, g, b = pixel_color(GRID[gy][gx], py, size)
            buf[idx] = r
            buf[idx + 1] = g
            buf[idx + 2] = b
            buf[idx + 3] = 255

    return bytes(buf)


def main():
    project_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    iconset_dir = os.path.join(project_dir, "assets", "AppIcon.iconset")
    icns_path = os.path.join(project_dir, "assets", "AppIcon.icns")

    os.makedirs(iconset_dir, exist_ok=True)

    variants = [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ]

    for filename, size in variants:
        pixels = render(size)
        data = make_png(size, size, pixels)
        with open(os.path.join(iconset_dir, filename), "wb") as f:
            f.write(data)
        print(f"  {filename} ({size}x{size})")

    subprocess.run(["iconutil", "-c", "icns", iconset_dir, "-o", icns_path], check=True)
    print(f"\n  -> {icns_path}")

    shutil.rmtree(iconset_dir)


if __name__ == "__main__":
    main()
