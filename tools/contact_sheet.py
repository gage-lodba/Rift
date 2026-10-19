#!/usr/bin/env python3
"""Compose the curated Rift screenshot contact sheet.

Layout: 3x3 grid — the archived old-UI reference plus the eight native preview
views — with labels, written to
`docs/vulkan-migration/screenshots/12-contact-sheet.png`.

Usage (from the repository root):
    python tools/contact_sheet.py

Inputs are the committed gallery images (so the sheet always matches the
gallery); requires Pillow.
"""

from __future__ import annotations

import os
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHOTS = os.path.join(ROOT, "docs", "vulkan-migration", "screenshots")
OUT = os.path.join(SHOTS, "12-contact-sheet.png")

# (label, relative path under screenshots/). The first cell is the archived
# legacy-UI reference capture; the rest are the native preview views.
CELLS = [
    ("Legacy UI (archived reference)", os.path.join("..", "reference-old-ui.png")),
    ("Home", "01-home.png"),
    ("Search", "04-search-songs.png"),
    ("Liked Songs", "02-liked-songs.png"),
    ("Playlist", "03-playlist.png"),
    ("Artist", "05-artist.png"),
    ("Artist songs", "06-artist-songs.png"),
    ("Album", "07-album.png"),
    ("Settings", "08-settings.png"),
]

CELL_W = 540
CELL_H = 344  # 1160x740 aspect
GAP = 20
MARGIN = 20
LABEL_H = 24
BG = (18, 18, 21)
FG = (200, 200, 208)


def font(size: int):
    try:
        return ImageFont.load_default(size=size)
    except TypeError:  # Pillow < 10.1
        return ImageFont.load_default()


def main() -> int:
    label_font = font(17)
    cols = 3
    rows = (len(CELLS) + cols - 1) // cols
    width = MARGIN * 2 + cols * CELL_W + (cols - 1) * GAP
    height = MARGIN * 2 + rows * (LABEL_H + CELL_H) + (rows - 1) * GAP
    sheet = Image.new("RGB", (width, height), BG)
    draw = ImageDraw.Draw(sheet)

    for index, (label, rel) in enumerate(CELLS):
        col = index % cols
        row = index // cols
        x = MARGIN + col * (CELL_W + GAP)
        y = MARGIN + row * (LABEL_H + CELL_H + GAP)
        path = os.path.join(SHOTS, rel)
        image = Image.open(path).convert("RGB").resize((CELL_W, CELL_H), Image.LANCZOS)
        sheet.paste(image, (x, y + LABEL_H))
        draw.text((x, y + 3), label, fill=FG, font=label_font)

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    sheet.save(OUT)
    print(f"wrote {OUT} ({sheet.size[0]}x{sheet.size[1]}) from {len(CELLS)} images")
    return 0


if __name__ == "__main__":
    sys.exit(main())
