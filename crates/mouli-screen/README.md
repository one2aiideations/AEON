# mouli-screen

The on-device screens as dependency-free state machines drawn into a character-cell grid:
Chat (F1), Models (F2), Approvals (F3), Audit (F4), Security (F5), Code review (F6).

`Grid` is rasterised with a public-domain 8x8 font into XRGB8888 or RGB565 pixels, or into a PNG
(`cargo run -p mouli-screen --example screenshots -- <dir>`). Screens report an `Action`; they never
act themselves. 87 tests, including draw-at-every-size sweeps that must not panic.

See `STATUS.md` for what has and has not been checked on real hardware.
