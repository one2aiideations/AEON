# MOULI-SCREEN status

On-device screens for MOULI, written as dependency-free Rust. A screen is a state machine that
takes key presses and draws into a grid of character cells. The grid is then drawn as pixels
(framebuffer or PNG) or read as text (tests).

## Built and tested

| Piece | What it does |
|---|---|
| `font` | 8x8 glyphs for ASCII, box drawing and block elements, generated from the public-domain `font8x8` headers. |
| `grid` | Cell grid, theme colours, boxes with titles, bars, text fitting and word wrapping; all drawing is clipped, never panics. |
| `raster` | Grid to pixels (scale, vertically centred text, unbroken box lines), 32-bit and 16-bit framebuffer byte layouts, and an exact PNG writer (checked with an independent decoder). |
| `key` | Terminal bytes to keys: arrows, function keys (xterm and Linux console forms), UTF-8, split input, runaway sequences. |
| `screens::Chat` | Transcript with labels, word wrap, scrolling, a real line editor, busy state. |
| `screens::ModelPicker` | Model list with memory fit, selection, shortcuts, empty-state help. |
| `screens::Approvals` | Held requests with a detail pane; approve or refuse; nobody can decide their own request (shown in red with the reason). |
| `screens::Audit` | The hash-chained log, newest first; green "chain verified" or a red "CHAIN BROKEN at record N"; outcomes coloured by meaning; filter to problems only; full outcome of the selected record. |
| `shell` | Header with six tabs (F1 to F6), footer hints, key routing. |

Run `cargo run -p mouli-screen --example screenshots -- <dir>` to write PNGs of the screens.

## Not built yet

* Security and Code review screens (those two tabs still show the model list).
* The device layer: reading `/dev/fb0` geometry and writing pixels, switching the keyboard to
  raw mode, and stopping the text console drawing over the screen.
* Connecting the screens to `mouli-interface`, `mouli-security` and `mouli-code`, and replacing
  `moulid`'s text console with this shell.
* Checking colours on real hardware: framebuffers differ in pixel format and this has only been
  checked as bytes, never on a screen.
