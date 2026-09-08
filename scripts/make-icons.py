#!/usr/bin/env python3
"""Builds the app icon set from the source artwork.

The source art is a glass badge sitting on a dark field. macOS composites its
own shadow and expects the badge itself with transparency around it, so the
badge is detected, cut out with a rounded-rect mask matching its own corner
radius, and centered on a transparent canvas at the proportions Apple's icon
grid expects.
"""
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

CANVAS = 1024
# Fraction of the canvas the badge fills. Apple's macOS grid leaves room
# around the art for the system-drawn shadow.
BADGE_FRACTION = 0.82
# The badge's corner radius as a fraction of its own size, measured from the
# source artwork's squircle.
CORNER_FRACTION = 0.235
# Supersampling factor for the mask, so the rounded edge is smooth.
SS = 4


def detect_badge(im: Image.Image) -> tuple[int, int, int, int]:
    a = np.asarray(im.convert("RGB")).astype(int)
    lum = a.sum(axis=2)
    threshold = a[10, 10].sum() + 26
    mask = lum > threshold

    def strong_span(axis: int) -> tuple[int, int]:
        counts = mask.sum(axis=axis)
        strong = np.where(counts > counts.max() * 0.35)[0]
        return int(strong.min()), int(strong.max())

    x0, x1 = strong_span(0)
    y0, y1 = strong_span(1)
    return x0, y0, x1 + 1, y1 + 1


def rounded_mask(size: int, radius: int) -> Image.Image:
    big = Image.new("L", (size * SS, size * SS), 0)
    ImageDraw.Draw(big).rounded_rectangle(
        (0, 0, size * SS - 1, size * SS - 1), radius=radius * SS, fill=255
    )
    return big.resize((size, size), Image.LANCZOS)


def build_master(source: Path) -> Image.Image:
    im = Image.open(source)
    box = detect_badge(im)
    badge = im.convert("RGB").crop(box)

    # Square it off, in case the detected badge is a pixel or two off.
    side = max(badge.size)
    squared = Image.new("RGB", (side, side))
    squared.paste(badge, ((side - badge.width) // 2, (side - badge.height) // 2))

    target = int(CANVAS * BADGE_FRACTION)
    squared = squared.resize((target, target), Image.LANCZOS)
    squared.putalpha(rounded_mask(target, int(target * CORNER_FRACTION)))

    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    offset = (CANVAS - target) // 2
    canvas.paste(squared, (offset, offset), squared)
    return canvas


def write_iconset(master: Image.Image, out: Path) -> None:
    iconset = out / "shell.iconset"
    iconset.mkdir(parents=True, exist_ok=True)
    for base in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            px = base * scale
            suffix = "" if scale == 1 else "@2x"
            name = f"icon_{base}x{base}{suffix}.png"
            master.resize((px, px), Image.LANCZOS).save(iconset / name)
    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset), "-o", str(out / "icon.icns")],
        check=True,
    )
    print(f"wrote {out / 'icon.icns'}")


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: make-icons.py <source-artwork.png>", file=sys.stderr)
        return 2

    ASSETS.mkdir(exist_ok=True)
    master = build_master(Path(sys.argv[1]))
    master.save(ASSETS / "icon.png")
    print(f"wrote {ASSETS / 'icon.png'} ({master.size[0]}x{master.size[1]}, RGBA)")

    # Linux desktop-entry icon sizes.
    for px in (256, 128, 64, 48, 32):
        master.resize((px, px), Image.LANCZOS).save(ASSETS / f"icon-{px}.png")

    if sys.platform == "darwin":
        write_iconset(master, ASSETS)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
