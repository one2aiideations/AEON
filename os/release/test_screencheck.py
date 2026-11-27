"""Tests for the pixel counting in os/tools/screencheck.py (the QEMU boot itself is CI only)."""
import importlib.util
import os
import re
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "..", "tools", "screencheck.py")
THEME = os.path.join(HERE, "..", "..", "crates", "mouli-desk", "src", "theme.rs")

spec = importlib.util.spec_from_file_location("screencheck", TOOL)
screencheck = importlib.util.module_from_spec(spec)
spec.loader.exec_module(screencheck)


class WallpaperShare(unittest.TestCase):
    def test_counts_only_exact_wallpaper_pixels(self):
        wall = bytes(screencheck.WALLPAPER)
        other = bytes([0x26, 0x32, 0x39])
        pix = wall * 3 + other
        self.assertAlmostEqual(screencheck.wallpaper_share(pix), 0.75)
        self.assertEqual(screencheck.wallpaper_share(b""), 0.0)
        self.assertEqual(screencheck.wallpaper_share(bytes(30)), 0.0)

    def test_lit_ignores_dark_pixels(self):
        self.assertEqual(screencheck.lit(bytes([0, 0, 0, 49, 0, 0, 48, 48, 48])), 1)

    def test_the_colour_matches_the_desktop_theme(self):
        src = open(THEME).read()
        m = re.search(r"pub const WALLPAPER: u32 = 0x([0-9A-Fa-f]{6});", src)
        self.assertIsNotNone(m)
        v = int(m.group(1), 16)
        self.assertEqual(screencheck.WALLPAPER, (v >> 16 & 255, v >> 8 & 255, v & 255))


if __name__ == "__main__":
    unittest.main()
