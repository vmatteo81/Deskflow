#!/usr/bin/env python3
"""Rasterizza assets/logo.svg in PNG e ICO (richiede librsvg via GdkPixbuf + Pillow)."""
import io, pathlib
import gi
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf
from PIL import Image

root = pathlib.Path(__file__).resolve().parent.parent / "assets"
SVG = str(root / "logo.svg")

def render(size):
    pb = GdkPixbuf.Pixbuf.new_from_file_at_size(SVG, size, size)
    ok, data = pb.save_to_bufferv("png", [], [])
    return Image.open(io.BytesIO(data)).convert("RGBA")

for s in (256, 512):
    render(s).save(root / f"ponte-{s}.png")
sizes = [16, 24, 32, 48, 64, 128, 256]
big = render(256)
big.save(root / "ponte.ico", sizes=[(s, s) for s in sizes],
         append_images=[render(s) for s in sizes[:-1]])
print("ok:", ", ".join(p.name for p in sorted(root.glob("ponte*"))))
