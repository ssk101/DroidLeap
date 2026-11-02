#!/usr/bin/env python3
"""Render icon.png: a laptop handing its mouse pointer to an Android tablet.

Kept to a few chunky shapes so it still reads as a 16-32 px tray icon.
"""
from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 256
SS = 4  # supersampling factor for anti-aliasing

INK = (38, 50, 56, 255)
LAPTOP_SCREEN = (79, 195, 247, 255)
LAPTOP_BASE = (120, 144, 156, 255)
TABLET_BODY = (61, 220, 132, 255)
TABLET_SCREEN = (232, 245, 233, 255)
POINTER = (255, 255, 255, 255)


def box(x0, y0, x1, y1):
    return [x0 * SS, y0 * SS, x1 * SS, y1 * SS]


def pts(*xy):
    return [(x * SS, y * SS) for x, y in xy]


def render():
    img = Image.new("RGBA", (SIZE * SS, SIZE * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Laptop, back left.
    d.rounded_rectangle(box(8, 44, 172, 166), radius=16 * SS, fill=INK)
    d.rounded_rectangle(box(24, 60, 156, 150), radius=6 * SS, fill=LAPTOP_SCREEN)
    d.polygon(pts((0, 160), (180, 160), (166, 194), (14, 194)), fill=INK)
    d.polygon(pts((14, 170), (166, 170), (159, 184), (21, 184)), fill=LAPTOP_BASE)

    # Tablet, front right, overlapping the laptop.
    d.rounded_rectangle(box(132, 76, 250, 248), radius=20 * SS, fill=INK)
    d.rounded_rectangle(box(142, 86, 240, 238), radius=13 * SS, fill=TABLET_BODY)
    d.rounded_rectangle(box(152, 100, 230, 224), radius=6 * SS, fill=TABLET_SCREEN)

    # Mouse pointer now living on the tablet.
    d.polygon(pts((168, 116), (168, 200), (187, 182), (200, 210), (216, 203),
                  (203, 175), (229, 175)),
              fill=POINTER, outline=INK, width=8 * SS)

    return img.resize((SIZE, SIZE), Image.LANCZOS)


if __name__ == "__main__":
    here = Path(__file__).parent
    icon = render()
    icon.save(here / "icon.png")
    # Windows picks the closest size per DPI: tray 16-32, Explorer 32-256.
    icon.save(here / "icon.ico",
              sizes=[(n, n) for n in (16, 20, 24, 32, 40, 48, 64, 256)])
    print(here / "icon.png", here / "icon.ico")
