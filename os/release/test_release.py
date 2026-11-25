"""Tests for textpdf.py and make-release.sh: python3 -m unittest discover -s os/release"""
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import textpdf  # noqa: E402

SCRIPT = os.path.join(HERE, "make-release.sh")


def read(path, mode="r"):
    with open(path, mode) as f:
        return f.read()


def objects_ok(pdf):
    """Every xref offset must point at 'N 0 obj', and startxref at the xref table."""
    m = re.search(rb"startxref\n(\d+)\n%%EOF\n$", pdf)
    assert m, "no startxref"
    xref = int(m.group(1))
    assert pdf[xref : xref + 4] == b"xref", "startxref does not point at xref"
    count = int(pdf[xref:].split(b"\n")[1].split()[1])
    rows = pdf[xref:].split(b"\n")[2 : 2 + count]
    assert rows[0].startswith(b"0000000000 65535 f")
    for n, row in enumerate(rows[1:], 1):
        off = int(row.split()[0])
        assert pdf[off:].startswith(f"{n} 0 obj\n".encode()), f"object {n} not at {off}"
    return count - 1


class TextPdf(unittest.TestCase):
    def render(self, text):
        return textpdf.render(text, "T", "A", "F")

    def test_the_file_structure_is_valid_for_one_and_for_many_pages(self):
        pdf, n = self.render("# Title\n\nhello\n")
        self.assertTrue(pdf.startswith(b"%PDF-1.4"))
        self.assertEqual(n, 1)
        self.assertEqual(objects_ok(pdf), 6 + 2 * n)
        long = "\n\n".join(f"Paragraph {i} " + "word " * 60 for i in range(40))
        pdf, n = self.render(long)
        self.assertGreater(n, 2)
        self.assertEqual(objects_ok(pdf), 6 + 2 * n)
        self.assertIn(f"/Count {n}".encode(), pdf)

    def test_the_same_input_gives_the_same_bytes(self):
        a, _ = self.render("# T\n\ntext (with) \\ marks\n")
        b, _ = self.render("# T\n\ntext (with) \\ marks\n")
        self.assertEqual(a, b)

    def test_special_characters_are_escaped_so_the_text_survives(self):
        pdf, _ = self.render("a (b) c \\ d\n")
        self.assertIn(b"a \\(b\\) c \\\\ d", pdf)

    def test_lines_never_run_past_the_right_margin(self):
        text = "W" * 200 + "\n\n" + " ".join(["WWWW MMMM"] * 40) + "\n\n- " + "mmmm " * 50
        for page in textpdf.layout(text):
            for font, size, x, _y, line in page:
                w = textpdf.width(line, size, bold=(font == "F2"), mono=(font == "F3"))
                self.assertLessEqual(x + w, textpdf.PAGE_W - textpdf.MARGIN_X + 0.5, line)

    def test_nothing_is_drawn_below_the_footer_line(self):
        long = "\n".join(f"- item {i} " + "x " * 30 for i in range(200))
        for page in textpdf.layout(long):
            for _f, _s, _x, y, _l in page:
                self.assertGreaterEqual(y, textpdf.MARGIN_BOTTOM - 1)

    def test_a_bullet_and_its_text_share_a_line_and_wrapped_text_is_indented(self):
        page = textpdf.layout("- " + "word " * 40)[0]
        self.assertEqual(page[0][4], "-")
        self.assertEqual(page[0][3], page[1][3], "same baseline")
        self.assertGreater(page[1][2], page[0][2])

    def test_non_ascii_and_oversized_code_lines_are_refused(self):
        with self.assertRaises(ValueError):
            textpdf.layout("café\n")
        with self.assertRaises(ValueError):
            textpdf.layout("    " + "x" * 200 + "\n")

    def test_empty_input_still_makes_a_valid_one_page_file(self):
        pdf, n = self.render("")
        self.assertEqual(n, 1)
        objects_ok(pdf)

    def test_a_long_word_is_broken_instead_of_overflowing(self):
        lines = textpdf.wrap("A" * 300, 10.5, 400)
        self.assertGreater(len(lines), 1)
        for l in lines:
            self.assertLessEqual(textpdf.width(l, 10.5), 400)


class MakeRelease(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.iso = os.path.join(self.tmp, "in.iso")
        with open(self.iso, "wb") as f:
            f.write(os.urandom(200_000))
        self.out = os.path.join(self.tmp, "out")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run_script(self, *args, env=None):
        e = dict(os.environ, RELEASE_DATE="2026-11-25")
        e.update(env or {})
        return subprocess.run(["sh", SCRIPT, *args], capture_output=True, text=True, env=e)

    def test_the_folder_has_exactly_the_five_parts_with_the_right_names(self):
        r = self.run_script(self.iso, self.out, "1.0")
        self.assertEqual(r.returncode, 0, r.stderr)
        root = os.path.join(self.out, "MOULI-OS-Release-v1.0")
        found = sorted(
            os.path.relpath(os.path.join(d, f), root) for d, _, fs in os.walk(root) for f in fs
        )
        self.assertEqual(
            found,
            [
                "01-ISO/MOULI-OS.iso",
                "02-Installation-Guide/Installation-Guide.pdf",
                "03-Verification/SHA256SUMS",
                "04-Release-Notes/Release-Notes.pdf",
                "05-Licensing/LICENSE.txt",
            ],
        )

    def test_checksums_cover_every_other_file_and_match_the_iso(self):
        self.run_script(self.iso, self.out)
        root = os.path.join(self.out, "MOULI-OS-Release-v1.0")
        sums = read(os.path.join(root, "03-Verification/SHA256SUMS")).splitlines()
        names = [l.split("  ", 1)[1] for l in sums]
        self.assertNotIn("03-Verification/SHA256SUMS", names)
        self.assertEqual(len(names), 4)
        want = hashlib.sha256(read(self.iso, "rb")).hexdigest()
        self.assertIn(f"{want}  01-ISO/MOULI-OS.iso", sums)
        check = subprocess.run(
            ["sha256sum", "-c", "03-Verification/SHA256SUMS"], cwd=root, capture_output=True, text=True
        )
        self.assertEqual(check.returncode, 0, check.stdout + check.stderr)

    def test_documents_carry_the_version_the_checksum_and_the_author_and_no_placeholders(self):
        self.run_script(self.iso, self.out, "2.3")
        root = os.path.join(self.out, "MOULI-OS-Release-v2.3")
        lic = read(os.path.join(root, "05-Licensing/LICENSE.txt"))
        self.assertIn("Version 2.3", lic)
        self.assertIn("Chandra Mouli Krishna Moorthy", lic)
        self.assertEqual(re.findall(r"@[A-Z0-9_]+@", lic), [])
        if shutil.which("pdftotext"):
            sha = hashlib.sha256(read(self.iso, "rb")).hexdigest()
            for pdf in ("02-Installation-Guide/Installation-Guide.pdf", "04-Release-Notes/Release-Notes.pdf"):
                text = subprocess.run(
                    ["pdftotext", os.path.join(root, pdf), "-"], capture_output=True, text=True
                ).stdout
                self.assertIn("2.3", text)
                self.assertIn(sha, text.replace("\n", ""))
                self.assertIn("cmklive@gmail.com", text)
                self.assertEqual(re.findall(r"@[A-Z0-9_]+@", text), [])

    def test_an_existing_folder_is_not_replaced_without_force(self):
        self.assertEqual(self.run_script(self.iso, self.out).returncode, 0)
        again = self.run_script(self.iso, self.out)
        self.assertNotEqual(again.returncode, 0)
        self.assertIn("FORCE=1", again.stderr)
        self.assertEqual(self.run_script(self.iso, self.out, env={"FORCE": "1"}).returncode, 0)

    def test_bad_input_is_refused_and_leaves_nothing_behind(self):
        empty = os.path.join(self.tmp, "empty.iso")
        open(empty, "wb").close()
        for args, env in [
            ((), None),
            ((os.path.join(self.tmp, "none.iso"), self.out), None),
            ((empty, self.out), None),
            ((self.iso, self.out, "v1"), None),
            ((self.iso, self.out, "1..0"), None),
            ((self.iso, self.out, ".1"), None),
            ((self.iso, self.out, "1.0"), {"PRODUCT_FILE": "bad name"}),
            ((self.iso, self.out, "1.0"), {"PRODUCT_FILE": "../x"}),
            ((self.iso, self.out, "1.0"), {"RELEASE_DATE": "yesterday"}),
        ]:
            r = self.run_script(*args, env=env)
            self.assertNotEqual(r.returncode, 0, (args, env))
        self.assertFalse(os.path.exists(self.out))

    def test_another_product_gets_its_own_names(self):
        r = self.run_script(
            self.iso, self.out, "1.0", env={"PRODUCT_NAME": "MOULI Mail", "PRODUCT_FILE": "MOULI-Mail"}
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(os.path.isfile(os.path.join(self.out, "MOULI-Mail-Release-v1.0/01-ISO/MOULI-Mail.iso")))


if __name__ == "__main__":
    unittest.main()
