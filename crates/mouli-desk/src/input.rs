//! Mouse input: what a mouse did, and decoders for the two byte streams Linux offers
//! (`/dev/input/mice` in PS/2 form, and evdev event records). Both decoders are plain functions
//! of bytes, so they are tested here without any hardware.

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// The main button.
    Left,
    /// The secondary button.
    Right,
}

/// One thing a mouse did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    /// Moved by this many pixels (right and down are positive).
    Move {
        /// Horizontal movement.
        dx: i32,
        /// Vertical movement.
        dy: i32,
    },
    /// A button went down.
    Down(Button),
    /// A button came up.
    Up(Button),
    /// The wheel turned; positive is away from the user (scroll up).
    Wheel(i32),
}

/// Decodes the PS/2 packets of `/dev/input/mice`: three bytes per packet, or four when the
/// kernel has switched the device to wheel mode.
#[derive(Debug, Clone)]
pub struct Ps2Decoder {
    four_byte: bool,
    buf: Vec<u8>,
    left: bool,
    right: bool,
}

impl Ps2Decoder {
    /// A decoder for 3-byte packets, or 4-byte packets when `four_byte` is true.
    pub fn new(four_byte: bool) -> Self {
        Self {
            four_byte,
            buf: Vec::new(),
            left: false,
            right: false,
        }
    }

    /// Feeds bytes (in any split) and returns the events they complete. A byte that cannot start
    /// a packet (its sync bit is clear) is skipped, so a lost byte only costs one packet.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<MouseEvent> {
        let size = if self.four_byte { 4 } else { 3 };
        let mut out = Vec::new();
        for &b in bytes {
            if self.buf.is_empty() && b & 0x08 == 0 {
                continue;
            }
            self.buf.push(b);
            if self.buf.len() < size {
                continue;
            }
            let p = std::mem::take(&mut self.buf);
            let flags = p[0];
            let left = flags & 1 != 0;
            let right = flags & 2 != 0;
            if left != self.left {
                out.push(if left {
                    MouseEvent::Down(Button::Left)
                } else {
                    MouseEvent::Up(Button::Left)
                });
                self.left = left;
            }
            if right != self.right {
                out.push(if right {
                    MouseEvent::Down(Button::Right)
                } else {
                    MouseEvent::Up(Button::Right)
                });
                self.right = right;
            }
            // Movement is 9-bit two's complement: the sign bits live in the flags byte. An
            // overflow flag means the movement is meaningless, so it is dropped.
            let dx = if flags & 0x40 != 0 {
                0
            } else {
                i32::from(p[1]) - ((i32::from(flags) << 4) & 0x100)
            };
            let dy = if flags & 0x80 != 0 {
                0
            } else {
                i32::from(p[2]) - ((i32::from(flags) << 3) & 0x100)
            };
            if dx != 0 || dy != 0 {
                out.push(MouseEvent::Move { dx, dy: -dy });
            }
            if size == 4 {
                let wheel = i32::from((p[3] & 0x0F) as i8 - if p[3] & 0x08 != 0 { 16 } else { 0 });
                if wheel != 0 {
                    out.push(MouseEvent::Wheel(-wheel));
                }
            }
        }
        out
    }
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_WHEEL: u16 = 8;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;

/// Decodes evdev `input_event` records. The record is 24 bytes on 64-bit kernels and 16 bytes on
/// 32-bit ones; the time stamp at the start is ignored.
#[derive(Debug, Clone)]
pub struct EvdevDecoder {
    size: usize,
    buf: Vec<u8>,
    dx: i32,
    dy: i32,
}

impl EvdevDecoder {
    /// A decoder for records of `size` bytes (24 or 16).
    pub fn new(size: usize) -> Self {
        Self {
            size: if size == 16 { 16 } else { 24 },
            buf: Vec::new(),
            dx: 0,
            dy: 0,
        }
    }

    /// Feeds bytes (in any split) and returns the events they complete. Movement is reported once
    /// per report (at the `SYN` record), so a diagonal move is one event.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<MouseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buf.len() >= self.size {
            let rec: Vec<u8> = self.buf.drain(..self.size).collect();
            let t = self.size - 8;
            let kind = u16::from_le_bytes([rec[t], rec[t + 1]]);
            let code = u16::from_le_bytes([rec[t + 2], rec[t + 3]]);
            let value = i32::from_le_bytes([rec[t + 4], rec[t + 5], rec[t + 6], rec[t + 7]]);
            match (kind, code) {
                (EV_REL, REL_X) => self.dx += value,
                (EV_REL, REL_Y) => self.dy += value,
                (EV_REL, REL_WHEEL) if value != 0 => out.push(MouseEvent::Wheel(value)),
                (EV_KEY, BTN_LEFT) | (EV_KEY, BTN_RIGHT) if value == 0 || value == 1 => {
                    if self.dx != 0 || self.dy != 0 {
                        out.push(MouseEvent::Move {
                            dx: std::mem::take(&mut self.dx),
                            dy: std::mem::take(&mut self.dy),
                        });
                    }
                    let button = if code == BTN_LEFT {
                        Button::Left
                    } else {
                        Button::Right
                    };
                    out.push(if value == 1 {
                        MouseEvent::Down(button)
                    } else {
                        MouseEvent::Up(button)
                    });
                }
                (EV_SYN, 0) if self.dx != 0 || self.dy != 0 => out.push(MouseEvent::Move {
                    dx: std::mem::take(&mut self.dx),
                    dy: std::mem::take(&mut self.dy),
                }),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps2_decodes_movement_buttons_and_signs() {
        let mut d = Ps2Decoder::new(false);
        // Move right 5, up 3 (PS/2 up is positive; the screen's y grows downward).
        assert_eq!(
            d.feed(&[0x08, 5, 3]),
            vec![MouseEvent::Move { dx: 5, dy: -3 }]
        );
        // Left button down with no movement, then up.
        assert_eq!(d.feed(&[0x09, 0, 0]), vec![MouseEvent::Down(Button::Left)]);
        assert_eq!(d.feed(&[0x08, 0, 0]), vec![MouseEvent::Up(Button::Left)]);
        // Negative movement: dx = -2 (0xFE with x sign bit), dy = -1 (0xFF with y sign bit).
        assert_eq!(
            d.feed(&[0x08 | 0x10 | 0x20, 0xFE, 0xFF]),
            vec![MouseEvent::Move { dx: -2, dy: 1 }]
        );
        // Right button.
        assert_eq!(d.feed(&[0x0A, 0, 0]), vec![MouseEvent::Down(Button::Right)]);
    }

    #[test]
    fn ps2_resynchronises_and_ignores_overflow() {
        let mut d = Ps2Decoder::new(false);
        // A stray byte with the sync bit clear is skipped; the packet after it still decodes.
        assert_eq!(
            d.feed(&[0x00, 0x08, 1, 0]),
            vec![MouseEvent::Move { dx: 1, dy: 0 }]
        );
        // Overflow in x drops the x movement.
        assert_eq!(
            d.feed(&[0x08 | 0x40, 100, 4]),
            vec![MouseEvent::Move { dx: 0, dy: -4 }]
        );
        // Packets split across reads.
        assert!(d.feed(&[0x08, 2]).is_empty());
        assert_eq!(d.feed(&[0]), vec![MouseEvent::Move { dx: 2, dy: 0 }]);
    }

    #[test]
    fn ps2_wheel_packets_have_a_fourth_byte() {
        let mut d = Ps2Decoder::new(true);
        assert_eq!(d.feed(&[0x08, 0, 0, 0x01]), vec![MouseEvent::Wheel(-1)]);
        assert_eq!(d.feed(&[0x08, 0, 0, 0x0F]), vec![MouseEvent::Wheel(1)]);
    }

    fn rec(kind: u16, code: u16, value: i32, size: usize) -> Vec<u8> {
        let mut v = vec![0u8; size - 8];
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&code.to_le_bytes());
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    #[test]
    fn evdev_reports_one_move_per_sync_and_orders_buttons_after_movement() {
        for size in [24, 16] {
            let mut d = EvdevDecoder::new(size);
            let mut bytes = rec(EV_REL, REL_X, 4, size);
            bytes.extend(rec(EV_REL, REL_Y, -2, size));
            bytes.extend(rec(EV_SYN, 0, 0, size));
            assert_eq!(
                d.feed(&bytes),
                vec![MouseEvent::Move { dx: 4, dy: -2 }],
                "{size}"
            );
            let mut press = rec(EV_REL, REL_X, 1, size);
            press.extend(rec(EV_KEY, BTN_LEFT, 1, size));
            press.extend(rec(EV_KEY, BTN_LEFT, 2, size)); // auto-repeat is ignored
            press.extend(rec(EV_KEY, BTN_LEFT, 0, size));
            assert_eq!(
                d.feed(&press),
                vec![
                    MouseEvent::Move { dx: 1, dy: 0 },
                    MouseEvent::Down(Button::Left),
                    MouseEvent::Up(Button::Left)
                ]
            );
            assert_eq!(
                d.feed(&rec(EV_REL, REL_WHEEL, 1, size)),
                vec![MouseEvent::Wheel(1)]
            );
        }
    }

    #[test]
    fn evdev_handles_split_reads_and_unknown_events() {
        let mut d = EvdevDecoder::new(24);
        let mut bytes = rec(EV_REL, REL_X, 3, 24);
        bytes.extend(rec(3, 0, 99, 24)); // an absolute-axis event this mouse code does not use
        bytes.extend(rec(EV_SYN, 0, 0, 24));
        let (a, b) = bytes.split_at(30);
        assert!(d.feed(a).is_empty());
        assert_eq!(d.feed(b), vec![MouseEvent::Move { dx: 3, dy: 0 }]);
        assert!(d.feed(&[1, 2, 3]).is_empty(), "a partial record waits");
    }
}
