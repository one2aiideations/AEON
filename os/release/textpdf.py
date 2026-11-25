#!/usr/bin/env python3
"""Turn a small plain-text format into a PDF, with the standard library only.

    textpdf.py INPUT.txt OUTPUT.pdf --title "Title" --author "Name" --footer "text"

Input format (ASCII only):
    # Heading            a large heading
    ## Heading           a smaller heading
    - item               a bullet (a line indented by two more spaces continues it)
    (4+ spaces) code     monospace, kept exactly, never re-wrapped
    anything else        a paragraph; consecutive lines are joined and wrapped
    blank line           ends a paragraph

The PDF uses the three standard fonts every reader has (Helvetica, Helvetica-Bold, Courier), so
nothing is embedded. The output is the same for the same input (no dates, no random ids).
"""
import argparse
import sys

PAGE_W, PAGE_H = 595, 842  # A4 in points
MARGIN_X, MARGIN_TOP, MARGIN_BOTTOM = 64, 70, 70
BODY, CODE, H1, H2 = 10.5, 9, 18, 13

# Helvetica advance widths in 1/1000 em for ASCII 32..126 (Adobe's standard metrics).
HELV = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
]
assert len(HELV) == 95
SAFETY = 1.04  # bold and rounding: wrap a little early rather than overflow


def width(text, size, bold=False, mono=False):
    if mono:
        return len(text) * 0.6 * size
    w = sum(HELV[ord(c) - 32] for c in text) / 1000 * size
    return w * (1.1 if bold else 1.0) * SAFETY


def wrap(text, size, max_w, bold=False):
    lines, cur = [], ""
    for word in text.split():
        # A single word longer than the line is broken by characters.
        while width(word, size, bold) > max_w:
            k = len(word)
            while k > 1 and width(word[:k], size, bold) > max_w:
                k -= 1
            if cur:
                lines.append(cur)
                cur = ""
            lines.append(word[:k])
            word = word[k:]
        trial = word if not cur else cur + " " + word
        if width(trial, size, bold) <= max_w:
            cur = trial
        else:
            lines.append(cur)
            cur = word
    if cur:
        lines.append(cur)
    return lines or [""]


def check_ascii(text):
    for n, line in enumerate(text.split("\n"), 1):
        for c in line:
            if not (32 <= ord(c) <= 126 or c == "\t"):
                raise ValueError(f"line {n}: only printable ASCII is allowed, found {c!r}")


def layout(text):
    """Returns pages, each a list of (font, size, x, text); y is assigned at emit time."""
    check_ascii(text)
    text = text.replace("\t", "    ")
    body_w = PAGE_W - 2 * MARGIN_X
    items = []  # (kind, font, size, x, line, space_before)

    def add(font, size, x, line, before=0.0, mono=False):
        items.append((font, size, x, line, before))

    lines = text.split("\n")
    i, para = 0, []

    def flush():
        nonlocal para
        if para:
            first = True
            for l in wrap(" ".join(para), BODY, body_w):
                before = 9 if items and items[-1][0] == "F3" else 5
                add("F1", BODY, MARGIN_X, l, before if first else 0)
                first = False
            para = []

    while i < len(lines):
        line = lines[i].rstrip()
        i += 1
        if not line.strip():
            flush()
        elif line.startswith("## "):
            flush()
            for k, l in enumerate(wrap(line[3:], H2, body_w, bold=True)):
                add("F2", H2, MARGIN_X, l, 12 if k == 0 else 0)
        elif line.startswith("# "):
            flush()
            for k, l in enumerate(wrap(line[2:], H1, body_w, bold=True)):
                add("F2", H1, MARGIN_X, l, 16 if k == 0 else 0)
        elif line.startswith("    "):
            flush()
            code = line[4:]
            if width(code, CODE, mono=True) > body_w - 12:
                raise ValueError(f"code line too long for the page: {code!r}")
            add("F3", CODE, MARGIN_X + 12, code, 0 if items and items[-1][0] == "F3" else 6)
        elif line.startswith("- "):
            flush()
            item = line[2:]
            while i < len(lines) and lines[i].startswith("  ") and not lines[i].startswith("    "):
                item += " " + lines[i].strip()
                i += 1
            wrapped = wrap(item, BODY, body_w - 16)
            for k, l in enumerate(wrapped):
                if k == 0:
                    add("F1", BODY, MARGIN_X + 4, "-", 3)
                    add("F1", BODY, MARGIN_X + 16, l, 0)
                else:
                    add("F1", BODY, MARGIN_X + 16, l, 0)
        else:
            para.append(line.strip())
    flush()

    # Place items on pages. The bullet marker shares a baseline with the next item.
    pages, page, y = [], [], PAGE_H - MARGIN_TOP
    k = 0
    while k < len(items):
        font, size, x, line, before = items[k]
        lead = size * 1.35
        same_line = line == "-" and font == "F1" and k + 1 < len(items) and items[k + 1][2] == MARGIN_X + 16
        need = before + lead
        if y - need < MARGIN_BOTTOM:
            pages.append(page)
            page, y = [], PAGE_H - MARGIN_TOP
            before = 0
        y -= before + lead
        page.append((font, size, x, y, line))
        if same_line:
            font2, size2, x2, line2, _ = items[k + 1]
            page.append((font2, size2, x2, y, line2))
            k += 1
        k += 1
    pages.append(page)
    return pages


def esc(s):
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def render(text, title, author, footer):
    pages = layout(text)
    n = len(pages)
    objs = []  # bytes of each object body, 1-based numbering
    objs.append(b"<< /Type /Catalog /Pages 2 0 R >>")
    kids = " ".join(f"{7 + 2 * p} 0 R" for p in range(n))
    objs.append(f"<< /Type /Pages /Kids [{kids}] /Count {n} >>".encode())
    for name in ("Helvetica", "Helvetica-Bold", "Courier"):
        objs.append(f"<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>".encode())
    objs.append(f"<< /Title ({esc(title)}) /Author ({esc(author)}) /Producer (MOULI textpdf) >>".encode())
    for p, page in enumerate(pages):
        parts = []
        for font, size, x, y, line in page:
            parts.append(f"BT /{font} {size} Tf {x:.2f} {y:.2f} Td ({esc(line)}) Tj ET")
        foot = f"{footer}   -   page {p + 1} of {n}"
        parts.append(f"BT /F1 8 Tf {MARGIN_X} 38 Td ({esc(foot)}) Tj ET")
        parts.append(f"{MARGIN_X} 50 m {PAGE_W - MARGIN_X} 50 l S")
        stream = "\n".join(parts).encode("latin-1")
        page_no = 7 + 2 * p
        objs.append(
            (
                f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] "
                f"/Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >> >> /Contents {page_no + 1} 0 R >>"
            ).encode()
        )
        objs.append(b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"\nendstream")
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for num, body in enumerate(objs, 1):
        offsets.append(len(out))
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n".encode() + b"0000000000 65535 f \n"
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out), n


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input")
    ap.add_argument("output")
    ap.add_argument("--title", default="Document")
    ap.add_argument("--author", default="")
    ap.add_argument("--footer", default="")
    a = ap.parse_args(argv)
    with open(a.input, encoding="ascii") as f:
        text = f.read()
    pdf, pages = render(text, a.title, a.author, a.footer)
    with open(a.output, "wb") as f:
        f.write(pdf)
    print(f"wrote {a.output}: {pages} page(s), {len(pdf)} bytes")


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (ValueError, OSError) as e:
        sys.exit(f"textpdf: {e}")
