# MOULI-SHELL status

The device layer under `mouli-screen`: it makes the screens run on a Linux framebuffer and
keyboard. No `unsafe`, no memory mapping, no ioctls.

## Built and tested

| Piece | What it does | How it was checked |
|---|---|---|
| `fb` | Reads geometry from `/sys/class/graphics/fb0` (size, depth, stride); writes 32-bit (BGRX or RGBX) and 16-bit (RGB565) pixels with plain writes at the right row offsets; skips rows that did not change. | A regular file stands in for `/dev/fb0`: byte positions, a stride wider than the row, both pixel orders, 16-bit, row diffing, refusal of bad sizes and depths. |
| `console` | Unbinds the kernel's framebuffer text console and stops its cursor blink while the screens run, and puts both back (also on drop, only undoing its own changes). | A fake sysfs tree. |
| `tty` | Raw keyboard input as keys (no echo, no line wait, Ctrl-C as a key); lone Escape handled; split sequences reassembled; closed terminal reported. | A real pseudo-terminal. |
| `runner` | Draw, show, wait for keys, pass actions to a handler, idle hook for clock and status, redraw. | Scripted keys and a fake display. |
| all together | Keys typed on a pseudo-terminal end up on the framebuffer file, byte for byte equal to an independent rendering. | `tests/e2e.rs`, 35 runs including under CPU load, no failures. |

## Try it on real hardware

From a text console (Ctrl-Alt-F3), not inside a desktop session:

    sudo cargo run -p mouli-shell --example demo

## Not done yet

* Never run on a real framebuffer: pixel order is assumed BGRX for 32-bit (the common case for EFI,
  VESA and virtual GPUs) with `PixelOrder::Rgbx` as the switch. A real screen may differ.
* The kernel could keep an older picture or stop `fbcon` from unbinding on some drivers; failures
  are reported as notes, not hidden, but the effect on hardware is unseen.
* Not yet used by `moulid`: the image still starts the text console.
* No mouse or touch input, and no key repeat or layout handling beyond what the kernel console
  provides.
