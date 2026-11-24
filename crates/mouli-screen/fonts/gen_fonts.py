#!/usr/bin/env python3
"""Builds the embedded glyph atlases in this folder from Google's Roboto and Roboto Mono.

Usage: gen_fonts.py <Roboto.ttf> <RobotoMono.ttf> <output folder>

The fonts come from https://github.com/google/fonts (ofl/roboto and ofl/robotomono), SIL Open Font
License 1.1; see OFL.txt. Requires Pillow. Each atlas holds anti-aliased 8-bit glyph bitmaps for
the characters in CHARS, so the Rust side needs no font parser or other dependency.

File format (little endian): "MFN1", u8 size, i8 ascent, u8 line height, u8 cell width,
u16 glyph count, then per glyph (sorted by code point) u32 code point, u8 advance, i8 left,
i8 top (distance above the baseline), u8 width, u8 height, u32 offset into the pixel blob;
the blob (width*height bytes of coverage per glyph) follows the table.
"""
import struct
import sys

from PIL import Image, ImageDraw, ImageFont

CHARS = (
    [chr(c) for c in range(0x20, 0x7F)]
    + [chr(c) for c in range(0xA0, 0x100)]
    + list("–—‘’“”•…€←↑→↓✓")
)

# name, source, size in pixels, variation axes (Roboto: weight, width; Roboto Mono: weight)
FACES = [
    ("ui_small", "ui", 12, [400, 100]),
    ("ui", "ui", 14, [400, 100]),
    ("ui_medium", "ui", 14, [500, 100]),
    ("mono", "mono", 16, [400]),
]


def build(path, size, axes):
    font = ImageFont.truetype(path, size)
    font.set_variation_by_axes(axes)
    notdef = font.getmask("￿", mode="L")
    notdef_bytes = bytes(notdef)
    asc, desc = font.getmetrics()
    glyphs = []
    blob = bytearray()
    for ch in CHARS:
        if ch != " " and bytes(font.getmask(ch, mode="L")) == notdef_bytes and ch != "￿":
            continue  # the font has no glyph and would draw its missing-character box
        adv = round(font.getlength(ch))
        left, top, right, bottom = font.getbbox(ch, anchor="ls")
        w, h = right - left, bottom - top
        data = b""
        if ch != " " and w > 0 and h > 0:
            im = Image.new("L", (w, h), 0)
            ImageDraw.Draw(im).text((-left, -top), ch, font=font, fill=255, anchor="ls")
            data = im.tobytes()
        else:
            w = h = 0
        glyphs.append((ord(ch), adv, left, -top, w, h, len(blob)))
        blob += data
    cell = round(font.getlength("0"))
    head = b"MFN1" + struct.pack("<BbBBH", size, asc, asc + desc, cell, len(glyphs))
    table = b"".join(struct.pack("<IBbbBBI", *g) for g in glyphs)
    return head + table + bytes(blob), len(glyphs), asc, desc, cell


def main():
    ui, mono, out = sys.argv[1:4]
    for name, src, size, axes in FACES:
        data, n, asc, desc, cell = build(ui if src == "ui" else mono, size, axes)
        with open(f"{out}/{name}.bin", "wb") as f:
            f.write(data)
        print(f"{name}: {size}px, {n} glyphs, ascent {asc}, descent {desc}, cell {cell}, {len(data)} bytes")


main()
