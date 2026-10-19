#!/usr/bin/env python3
"""Compare a rendered Rift screenshot against the reference UI.

Usage:
    python tools/compare_images.py REFERENCE NEW [--out DIR] [--label NAME]

Writes three PNGs into DIR (default: artifacts/compare):
* side_by_side.png  — reference | new, scaled to the same width
* diff.png          — per-pixel absolute difference, amplified 4x
* overlay.png       — 50/50 blend, to spot layout drift
and prints a short report (mean/max channel delta, percent of pixels that
differ by more than a small threshold). Requires Pillow + numpy.

The reference is `Previews/image.png` (1160x740). Both images are resized to
1160x740 before comparison if needed, so a HiDPI capture still compares.
"""

from __future__ import annotations

import argparse
import os
import sys

import numpy as np
from PIL import Image, ImageChops


def load_rgb(path: str, size: tuple[int, int]) -> Image.Image:
    image = Image.open(path).convert("RGB")
    if image.size != size:
        image = image.resize(size, Image.LANCZOS)
    return image


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("reference")
    parser.add_argument("candidate")
    parser.add_argument("--out", default="artifacts/compare")
    parser.add_argument("--width", type=int, default=1160)
    parser.add_argument("--height", type=int, default=740)
    parser.add_argument("--label", default="compare")
    args = parser.parse_args()

    size = (args.width, args.height)
    reference = load_rgb(args.reference, size)
    candidate = load_rgb(args.candidate, size)

    ref = np.asarray(reference, dtype=np.int16)
    new = np.asarray(candidate, dtype=np.int16)
    delta = np.abs(ref - new)
    mean = float(delta.mean())
    max_delta = int(delta.max())
    per_pixel = delta.max(axis=2)
    changed = float((per_pixel > 16).mean() * 100.0)

    os.makedirs(args.out, exist_ok=True)
    base = os.path.join(args.out, args.label)

    # Side by side with a 2 px divider.
    canvas = Image.new("RGB", (size[0] * 2 + 2, size[1]), (30, 30, 34))
    canvas.paste(reference, (0, 0))
    canvas.paste(candidate, (size[0] + 2, 0))
    canvas.save(base + "_side_by_side.png")

    # Amplified diff.
    diff = np.clip(delta * 4, 0, 255).astype(np.uint8)
    Image.fromarray(diff, "RGB").save(base + "_diff.png")

    # 50/50 overlay.
    Image.blend(reference, candidate, 0.5).save(base + "_overlay.png")

    print(f"reference : {args.reference}")
    print(f"candidate : {args.candidate}")
    print(f"size      : {size[0]}x{size[1]}")
    print(f"mean |dc| : {mean:.2f} (0 = identical)")
    print(f"max  |dc| : {max_delta}")
    print(f"pixels >16: {changed:.2f}%")
    print(f"artifacts : {base}_side_by_side.png, {base}_diff.png, {base}_overlay.png")
    return 0


if __name__ == "__main__":
    sys.exit(main())
