#!/usr/bin/env python3
"""Draws the forhunt-viewer icon (original artwork: a timeline grid with a
highlighted row and a lens) and writes PNG / ICO / ICNS files.

usage: python3 scripts/make-icons.py   (needs Pillow)
"""
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "packaging" / "icons"
S = 1024


def draw(size=S):
    im = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    # Rounded square background.
    d.rounded_rectangle((40, 40, S - 40, S - 40), radius=200, fill=(22, 30, 40, 255))
    # Grid rows.
    x0, x1 = 170, S - 170
    for i, y in enumerate(range(230, 800, 95)):
        color = (0, 170, 160, 255) if i == 2 else (70, 86, 104, 255)
        d.rounded_rectangle((x0, y, x1, y + 52), radius=18, fill=color)
    # Column separators.
    for x in (330, 560):
        d.rectangle((x, 220, x + 10, 800), fill=(22, 30, 40, 255))
    # Lens over the highlighted row.
    cx, cy, r = 640, 470, 150
    d.ellipse((cx - r, cy - r, cx + r, cy + r), outline=(240, 200, 60, 255), width=46)
    d.line((cx + 105, cy + 105, cx + 250, cy + 250), fill=(240, 200, 60, 255), width=70)
    return im.resize((size, size), Image.LANCZOS)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    big = draw()
    big.save(OUT / "icon-1024.png")
    for s in (512, 256, 128, 64, 32):
        big.resize((s, s), Image.LANCZOS).save(OUT / f"icon-{s}.png")
    big.save(OUT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    big.save(OUT / "icon.icns")
    # Raw RGBA for the window icon (no image decoder in the binary).
    (OUT / "icon-128.rgba").write_bytes(big.resize((128, 128), Image.LANCZOS).tobytes())
    print("wrote", sorted(p.name for p in OUT.iterdir()))


if __name__ == "__main__":
    main()
