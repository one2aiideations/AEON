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

const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const BTN_TOUCH: u16 = 0x14a;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOOL_DOUBLETAP: u16 = 0x14d;

/// The smallest touchpad width (in device units) assumed when scaling, so the first swipes do not
/// fly across the screen before the real range has been seen. A pad wider than this makes the
/// first sweep overshoot a little; after one sweep from edge to edge the scale is right.
const MIN_SPAN: i32 = 4000;
/// A touch shorter than this (milliseconds) that barely moves counts as a tap.
const TAP_MS: u64 = 250;
/// Movement (in pixels) allowed during a tap.
const TAP_SLOP: i32 = 6;
/// Pixels of two-finger movement for one wheel step.
const SCROLL_STEP: i32 = 24;

/// Decodes the evdev records of a touchpad (absolute finger positions) into mouse events.
///
/// One finger moves the pointer, a short touch is a tap (a left click), the pad's own button
/// (`BTN_LEFT`) clicks, and two fingers moving up or down scroll. The pad's size is not known
/// without an ioctl, so the scale is learned from the positions seen: sweeping the whole pad
/// moves the pointer across the whole screen.
#[derive(Debug, Clone)]
pub struct TouchpadDecoder {
    size: usize,
    buf: Vec<u8>,
    screen_w: i32,
    x: Option<i32>,
    y: Option<i32>,
    last: Option<(i32, i32)>,
    saw_single_touch_axes: bool,
    min_x: i32,
    max_x: i32,
    touch: bool,
    finger: bool,
    two: bool,
    fingers_before: u8,
    down_at: Option<u64>,
    moved: i32,
    max_fingers: u8,
    clicked: bool,
    frac_x: f64,
    frac_y: f64,
    scroll_acc: i32,
    now_ms: u64,
}

impl TouchpadDecoder {
    /// A decoder for records of `size` bytes (24 or 16) on a screen `screen_w` pixels wide.
    pub fn new(size: usize, screen_w: i32) -> Self {
        Self {
            size: if size == 16 { 16 } else { 24 },
            buf: Vec::new(),
            screen_w: screen_w.max(1),
            x: None,
            y: None,
            last: None,
            saw_single_touch_axes: false,
            min_x: i32::MAX,
            max_x: i32::MIN,
            touch: false,
            finger: false,
            two: false,
            fingers_before: 0,
            down_at: None,
            moved: 0,
            max_fingers: 0,
            clicked: false,
            frac_x: 0.0,
            frac_y: 0.0,
            scroll_acc: 0,
            now_ms: 0,
        }
    }

    fn touching(&self) -> bool {
        self.touch || self.finger || self.two
    }

    fn fingers(&self) -> u8 {
        if self.two {
            2
        } else {
            u8::from(self.touching())
        }
    }

    /// Pixels per device unit, from the widest range of positions seen so far.
    fn gain(&self) -> f64 {
        let span = if self.max_x > self.min_x {
            (self.max_x - self.min_x).max(MIN_SPAN)
        } else {
            MIN_SPAN
        };
        f64::from(self.screen_w) / f64::from(span)
    }

    /// Feeds bytes (in any split) and returns the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<MouseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buf.len() >= self.size {
            let rec: Vec<u8> = self.buf.drain(..self.size).collect();
            let sec = u64::from_le_bytes(rec[0..8].try_into().unwrap_or([0; 8]));
            let usec = if self.size == 24 {
                u64::from_le_bytes(rec[8..16].try_into().unwrap_or([0; 8]))
            } else {
                0
            };
            self.now_ms = sec.saturating_mul(1000).saturating_add(usec / 1000);
            let t = self.size - 8;
            let kind = u16::from_le_bytes([rec[t], rec[t + 1]]);
            let code = u16::from_le_bytes([rec[t + 2], rec[t + 3]]);
            let value = i32::from_le_bytes([rec[t + 4], rec[t + 5], rec[t + 6], rec[t + 7]]);
            match (kind, code) {
                (EV_ABS, ABS_X) => {
                    self.saw_single_touch_axes = true;
                    self.x = Some(value);
                }
                (EV_ABS, ABS_Y) => {
                    self.saw_single_touch_axes = true;
                    self.y = Some(value);
                }
                (EV_ABS, ABS_MT_POSITION_X) if !self.saw_single_touch_axes => self.x = Some(value),
                (EV_ABS, ABS_MT_POSITION_Y) if !self.saw_single_touch_axes => self.y = Some(value),
                (EV_KEY, BTN_TOUCH) => self.touch = value != 0,
                (EV_KEY, BTN_TOOL_FINGER) => self.finger = value != 0,
                (EV_KEY, BTN_TOOL_DOUBLETAP) => self.two = value != 0,
                (EV_KEY, BTN_LEFT) if value == 0 || value == 1 => {
                    self.clicked = true;
                    out.push(if value == 1 {
                        MouseEvent::Down(Button::Left)
                    } else {
                        MouseEvent::Up(Button::Left)
                    });
                }
                (EV_SYN, 0) => self.sync(&mut out),
                _ => {}
            }
        }
        out
    }

    fn sync(&mut self, out: &mut Vec<MouseEvent>) {
        let fingers = self.fingers();
        if fingers != self.fingers_before {
            if self.fingers_before == 0 {
                // A new touch begins.
                self.down_at = Some(self.now_ms);
                self.moved = 0;
                self.max_fingers = 0;
                self.clicked = false;
                self.scroll_acc = 0;
            }
            // A change in the number of fingers makes the reported position jump; start afresh.
            self.last = None;
            if fingers == 0 {
                self.finish_touch(out);
            }
            self.fingers_before = fingers;
        }
        self.max_fingers = self.max_fingers.max(fingers);
        if fingers == 0 {
            self.x = None;
            self.y = None;
            return;
        }
        let (Some(x), Some(y)) = (self.x, self.y) else {
            return;
        };
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        if let Some((lx, ly)) = self.last {
            let (rx, ry) = (x - lx, y - ly);
            let g = self.gain();
            if fingers >= 2 {
                self.scroll_acc += (f64::from(ry) * g) as i32;
                while self.scroll_acc.abs() >= SCROLL_STEP {
                    let step = self.scroll_acc.signum();
                    self.scroll_acc -= step * SCROLL_STEP;
                    // Content follows the fingers: moving down scrolls up.
                    out.push(MouseEvent::Wheel(step));
                }
                self.moved += (f64::from(rx.abs() + ry.abs()) * g) as i32;
            } else {
                self.frac_x += f64::from(rx) * g;
                self.frac_y += f64::from(ry) * g;
                let (dx, dy) = (self.frac_x as i32, self.frac_y as i32);
                self.frac_x -= f64::from(dx);
                self.frac_y -= f64::from(dy);
                self.moved += dx.abs() + dy.abs();
                if dx != 0 || dy != 0 {
                    out.push(MouseEvent::Move { dx, dy });
                }
            }
        }
        self.last = Some((x, y));
    }

    fn finish_touch(&mut self, out: &mut Vec<MouseEvent>) {
        let quick = self
            .down_at
            .is_some_and(|t| self.now_ms.saturating_sub(t) <= TAP_MS);
        if quick && self.moved <= TAP_SLOP && self.max_fingers == 1 && !self.clicked {
            out.push(MouseEvent::Down(Button::Left));
            out.push(MouseEvent::Up(Button::Left));
        }
        self.down_at = None;
        self.frac_x = 0.0;
        self.frac_y = 0.0;
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

    fn trec(ms: u64, kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(ms / 1000).to_le_bytes());
        v.extend_from_slice(&((ms % 1000) * 1000).to_le_bytes());
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&code.to_le_bytes());
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    fn frame(ms: u64, x: i32, y: i32, fingers: u8) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(trec(ms, EV_ABS, ABS_X, x));
        b.extend(trec(ms, EV_ABS, ABS_Y, y));
        b.extend(trec(ms, EV_KEY, BTN_TOUCH, 1));
        b.extend(trec(ms, EV_KEY, BTN_TOOL_FINGER, i32::from(fingers == 1)));
        b.extend(trec(
            ms,
            EV_KEY,
            BTN_TOOL_DOUBLETAP,
            i32::from(fingers == 2),
        ));
        b.extend(trec(ms, EV_SYN, 0, 0));
        b
    }

    fn lift(ms: u64) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(trec(ms, EV_KEY, BTN_TOUCH, 0));
        b.extend(trec(ms, EV_KEY, BTN_TOOL_FINGER, 0));
        b.extend(trec(ms, EV_KEY, BTN_TOOL_DOUBLETAP, 0));
        b.extend(trec(ms, EV_SYN, 0, 0));
        b
    }

    fn moves(ev: &[MouseEvent]) -> (i32, i32) {
        ev.iter().fold((0, 0), |(x, y), e| match e {
            MouseEvent::Move { dx, dy } => (x + dx, y + dy),
            _ => (x, y),
        })
    }

    #[test]
    fn a_finger_drag_moves_the_pointer_and_the_first_sample_does_not_jump() {
        let mut d = TouchpadDecoder::new(24, 1000);
        // First contact: no movement however far from the origin it lands.
        assert!(d.feed(&frame(0, 5000, 3000, 1)).is_empty());
        // The scale starts at the screen width per MIN_SPAN units: 1000 px / 4000 units.
        let ev = d.feed(&frame(10, 5400, 3200, 1));
        let (dx, dy) = moves(&ev);
        assert!((95..=105).contains(&dx), "{dx}");
        assert!((45..=55).contains(&dy), "{dy}");
        let ev = d.feed(&frame(20, 5400, 3000, 1));
        assert!(moves(&ev).1 < 0);
    }

    #[test]
    fn after_one_sweep_a_full_pad_width_crosses_the_screen_once() {
        let mut d = TouchpadDecoder::new(24, 1000);
        d.feed(&frame(0, 0, 0, 1));
        // The learning sweep: left edge to right edge of an 8000-unit pad.
        for step in 1..=10 {
            d.feed(&frame(step * 10, (step * 800) as i32, 0, 1));
        }
        // The way back, now that the width is known.
        let mut back = 0;
        for step in 1..=10 {
            back += moves(&d.feed(&frame(200 + step * 10, (8000 - step * 800) as i32, 0, 1))).0;
        }
        assert!((-1150..=-850).contains(&back), "{back}");
    }

    #[test]
    fn a_quick_still_touch_is_a_tap_and_a_long_or_moving_one_is_not() {
        let mut d = TouchpadDecoder::new(24, 1000);
        d.feed(&frame(0, 100, 100, 1));
        let ev = d.feed(&lift(100));
        assert_eq!(
            ev,
            vec![MouseEvent::Down(Button::Left), MouseEvent::Up(Button::Left)]
        );
        // Held too long.
        d.feed(&frame(1000, 100, 100, 1));
        assert!(d.feed(&lift(1500)).is_empty());
        // Moved too far.
        d.feed(&frame(2000, 100, 100, 1));
        d.feed(&frame(2010, 300, 100, 1));
        assert!(d.feed(&lift(2050)).is_empty());
    }

    #[test]
    fn the_physical_click_works_and_suppresses_the_tap() {
        let mut d = TouchpadDecoder::new(24, 1000);
        d.feed(&frame(0, 100, 100, 1));
        let mut b = trec(20, EV_KEY, BTN_LEFT, 1);
        b.extend(trec(60, EV_KEY, BTN_LEFT, 0));
        let ev = d.feed(&b);
        assert_eq!(
            ev,
            vec![MouseEvent::Down(Button::Left), MouseEvent::Up(Button::Left)]
        );
        assert!(d.feed(&lift(80)).is_empty(), "no second click from the tap");
    }

    #[test]
    fn two_fingers_scroll_and_do_not_move_the_pointer_or_tap() {
        let mut d = TouchpadDecoder::new(24, 1000);
        d.feed(&frame(0, 100, 100, 2));
        let mut wheel = 0;
        let mut other = 0;
        for i in 1..=5 {
            for e in d.feed(&frame(i * 10, 100, 100 + (i as i32) * 60, 2)) {
                match e {
                    MouseEvent::Wheel(n) => wheel += n,
                    _ => other += 1,
                }
            }
        }
        assert!(wheel >= 2, "scrolled {wheel} steps");
        assert_eq!(other, 0);
        assert!(d.feed(&lift(100)).is_empty());
    }

    #[test]
    fn changing_the_finger_count_does_not_make_the_pointer_jump() {
        let mut d = TouchpadDecoder::new(24, 1000);
        d.feed(&frame(0, 100, 100, 1));
        d.feed(&frame(10, 110, 100, 1));
        // A second finger lands and the reported position leaps.
        let ev = d.feed(&frame(20, 900, 700, 2));
        assert_eq!(moves(&ev), (0, 0));
    }

    #[test]
    fn touchpad_records_split_across_reads_and_noise_are_harmless() {
        let mut d = TouchpadDecoder::new(24, 1000);
        let bytes = frame(0, 100, 100, 1);
        let (a, b) = bytes.split_at(37);
        assert!(d.feed(a).is_empty());
        assert!(d.feed(b).is_empty());
        let ev = d.feed(&frame(10, 150, 100, 1));
        let (dx, dy) = moves(&ev);
        assert!(dx > 0 && dy == 0, "{dx},{dy}");
        assert!(d.feed(&[0xFF; 5]).is_empty());
        let _ = d.feed(&[0xFF; 240]);
        let mut small = TouchpadDecoder::new(16, 0);
        let _ = small.feed(&[0u8; 64]);
    }

    #[test]
    fn multitouch_only_pads_work_from_the_mt_axes() {
        let mut d = TouchpadDecoder::new(24, 1000);
        let mt = |ms: u64, x: i32, y: i32| {
            let mut b = trec(ms, EV_ABS, ABS_MT_POSITION_X, x);
            b.extend(trec(ms, EV_ABS, ABS_MT_POSITION_Y, y));
            b.extend(trec(ms, EV_KEY, BTN_TOUCH, 1));
            b.extend(trec(ms, EV_KEY, BTN_TOOL_FINGER, 1));
            b.extend(trec(ms, EV_SYN, 0, 0));
            b
        };
        assert!(d.feed(&mt(0, 100, 100)).is_empty());
        let (dx, dy) = moves(&d.feed(&mt(10, 1100, 100)));
        assert!(dx > 0 && dy == 0, "{dx},{dy}");
    }
}
