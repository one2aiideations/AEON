# mouli-desk

A classic desktop for the MOULI device (windows, menu bar, icons, pointer) drawn in software into
a pixel canvas: no windowing system, no GPU. Look and feel follow Google's Material Design: dark
surfaces that lighten with elevation, soft shadows, Roboto text, and the Material colour palette.

- **Windows** with title bars, close and zoom controls, a grow handle and a hint strip. A window shows one of the eight MOULI screens, unchanged, drawn in Roboto Mono at 10 by 22 pixels per cell, so windows resize in whole cells.
- **Menu bar** (MOULI, File, Edit, View, Window, Help) with pull-down menus, shortcuts, greyed-out items, and press-drag-release selection.
- **Desktop icons** (double click to open), an **About** box and a **Help** box (modal), a pixel-art **pointer**. Pixel-art icons are still a placeholder for proper vector icons.
- **Window manager**: front window gets the keys, dragging cannot lose a title bar, zoom restores exactly, tile never overlaps (it cascades when the windows cannot fit).
- **Mouse decoding** for `/dev/input/mice` (PS/2, 3 or 4 byte packets) and evdev records (24 or 16 byte), as plain functions over bytes.

`cargo run -p mouli-desk --example desktop -- <dir>` writes PNGs of a populated desktop.
45 tests cover window geometry, hit testing, every mouse interaction, the menus, the decoders and
the rendered pixels.

## Not done yet

- Nothing reads a real mouse: the kernel image needs mouse input enabled, which is an OS change
  held for the build window. The decoders are tested against byte streams only.
- `mouli-device` and `mouli-shell` still run the keyboard-only layout; the desktop is not yet
  their front end.
- Clicking inside a window only focuses it (and the wheel scrolls); the screens do not take mouse
  clicks on their rows yet.
- Supported screen sizes are 640 by 480 and up. Looks have only been checked on rendered images,
  not on a real display.
