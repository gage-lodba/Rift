#!/usr/bin/env python3
"""Regenerate the Rift titlebar logo texture from `logo.svg`.

The old web UI loaded the legacy logo SVG with `<img class="logo-mark">` and CSS
`filter: drop-shadow(0 0 3px rgba(157,92,255,.55))`. The native PaintList has no
SVG renderer and no blur, so the mark is prerasterized once into
`logo@2x.png`:

* the tile itself is rendered from the real SVG at 44x44 (2x of the 22 px
  logical size) with librsvg (`rsvg-convert`, cairo backend — matches the old
  WebView's SVG gradients/clip/filter closely);
* the CSS drop-shadow is baked underneath it as a blurred violet halo, on a
  64x64 (32 logical px) canvas: the tile occupies (10,10)..(53,53) i.e. a 5 px
  logical margin on each side.

Usage (requires `rsvg-convert` + Pillow + numpy):
    python crates/rift-desktop/assets/logo/generate.py [--rsvg PATH]

Run it from the repository root; `logo.svg` and `logo@2x.png` live next to this
script.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
SVG = os.path.join(HERE, "logo.svg")
OUT = os.path.join(HERE, "logo@2x.png")

# 2x of the 32 logical px canvas: 22 px tile + 5 px glow margin each side.
CANVAS = 64
TILE = 44
INSET = 10
# CSS drop-shadow: 0 0 3px rgba(157, 92, 255, 0.55). Gaussian sigma = radius / 2,
# doubled for the 2x texture.
GLOW_SIGMA = 3.0
GLOW_COLOR = (157, 92, 255)
GLOW_ALPHA = 0.55


def find_rsvg(explicit: str | None) -> str:
    if explicit:
        return explicit
    candidates = [
        shutil.which("rsvg-convert"),
        r"C:\msys64\ucrt64\bin\rsvg-convert.exe",
        r"C:\msys64\mingw64\bin\rsvg-convert.exe",
        "/usr/bin/rsvg-convert",
        "/usr/local/bin/rsvg-convert",
    ]
    for candidate in candidates:
        if candidate and os.path.exists(candidate):
            return candidate
    raise SystemExit(
        "rsvg-convert not found; install librsvg or pass --rsvg <path>"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rsvg", default=None, help="path to rsvg-convert")
    args = parser.parse_args()
    rsvg = find_rsvg(args.rsvg)

    with tempfile.TemporaryDirectory() as tmp:
        tile_path = os.path.join(tmp, "tile.png")
        subprocess.run(
            [rsvg, "-w", str(TILE), "-h", str(TILE), "-o", tile_path, SVG],
            check=True,
        )
        tile = Image.open(tile_path).convert("RGBA")
    if tile.size != (TILE, TILE):
        raise SystemExit(f"unexpected tile size {tile.size}")

    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))

    # ---- drop-shadow halo: blur the tile's alpha on a full-canvas layer so the
    # glow can spread into the margin, then tint it violet ----
    halo = Image.new("RGBA", (CANVAS, CANVAS), GLOW_COLOR + (0,))
    alpha_canvas = Image.new("L", (CANVAS, CANVAS), 0)
    alpha_canvas.paste(tile.getchannel("A"), (INSET, INSET))
    alpha_canvas = alpha_canvas.filter(ImageFilter.GaussianBlur(GLOW_SIGMA))
    halo.putalpha(alpha_canvas.point(lambda a: int(a * GLOW_ALPHA)))
    canvas.alpha_composite(halo)

    # ---- the real SVG tile on top ----
    canvas.alpha_composite(tile, (INSET, INSET))

    canvas.save(OUT)
    print(f"wrote {OUT} ({canvas.size[0]}x{canvas.size[1]})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
