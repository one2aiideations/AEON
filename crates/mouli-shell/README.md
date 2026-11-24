# mouli-shell

Runs the `mouli-screen` interface on a Linux framebuffer with a raw keyboard: framebuffer info from
sysfs, row-diff presenting, taking over the console from fbcon, raw terminal input, and a generic
run loop. Uses `nix` for termios and polling.

31 tests including pty-based input and a byte-exact end-to-end frame check. Colour order on real
hardware is unverified (`MOULI_FB_ORDER` selects it); see `STATUS.md`.
