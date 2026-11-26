//! The graphical desktop on the firmware framebuffer.
//!
//! `moulid` draws the MOULI desktop (`mouli-desk`) straight into `/dev/fb0`, reads the keyboard and
//! mouse from the kernel's evdev devices (`/dev/input/event*`) and runs the chat inside the desktop
//! window. No `unsafe` code is needed: the screen's size comes from `/sys/class/graphics/fb0`, pixels
//! are written to the device file, and input records are read as bytes.
//!
//! If anything is missing (no framebuffer, a screen that is not 32 bits per pixel, no keyboard
//! device) [`start`] returns an error and `moulid` keeps the text chat. A pointing device that
//! reports absolute positions (some touchpads and touchscreens) is not used; a USB or PS/2 mouse is.

use crate::datagraph::Recorder;
use crate::sysinfo::{self, Collector, SysEnv};
use crate::toolloop;
use crate::ui::{trim_history, Backend, End, Turn};
use crate::wifi::Runner;
use mouli_desk::desktop::Desktop;
use mouli_desk::input::{EvdevDecoder, MouseEvent, TouchpadDecoder};
use mouli_desk::status::SystemStatus;
use mouli_device::{Backend as DeviceBackend, Device, Outcome};
use mouli_interface::routing::{ModelEntry, Route};
use mouli_interface::{Permissions, Policy, Workspace};
use mouli_screen::key::Key;
use mouli_screen::raster::Canvas;
use mouli_screen::screen::{Action, Tab};
use mouli_screen::screens::{ModelRow, Role};
use mouli_screen::shell::Shell;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The framebuffer device.
pub const FB_DEVICE: &str = "/dev/fb0";
/// Where the kernel describes the framebuffer.
pub const FB_SYS: &str = "/sys/class/graphics/fb0";
/// Where the kernel offers input devices.
pub const INPUT_DIR: &str = "/dev/input";
/// Where the kernel describes input devices.
pub const INPUT_SYS: &str = "/sys/class/input";

/// The smallest screen the desktop can use.
const MIN_W: usize = 640;
const MIN_H: usize = 480;
/// A sanity limit on a reported screen size.
const MAX_DIM: usize = 16_384;
/// Bytes in one evdev record on a 64-bit kernel.
const EV_SIZE: usize = 24;

/// The geometry of the framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbInfo {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Bytes from the start of one row to the next.
    pub stride: usize,
}

/// Reads the framebuffer geometry from the text the kernel gives in `virtual_size` (`1440,900`),
/// `stride` (bytes per row) and `bits_per_pixel`.
///
/// # Errors
/// A message when the text is malformed, the screen is too small or too large, or is not 32 bits
/// per pixel (the only layout drawn here).
pub fn parse_fb_info(virtual_size: &str, stride: &str, bits: &str) -> Result<FbInfo, String> {
    let (w, h) = virtual_size
        .trim()
        .split_once(',')
        .ok_or_else(|| format!("unreadable screen size {:?}", virtual_size.trim()))?;
    let num = |s: &str, what: &str| {
        s.trim()
            .parse::<usize>()
            .map_err(|_| format!("unreadable {what} {:?}", s.trim()))
    };
    let (width, height) = (num(w, "width")?, num(h, "height")?);
    let stride = num(stride, "stride")?;
    let bits = num(bits, "bits per pixel")?;
    if bits != 32 {
        return Err(format!(
            "the screen uses {bits} bits per pixel; 32 is needed"
        ));
    }
    if !(MIN_W..=MAX_DIM).contains(&width) || !(MIN_H..=MAX_DIM).contains(&height) {
        return Err(format!("screen size {width}x{height} is not usable"));
    }
    if stride < width * 4 || stride > MAX_DIM * 4 {
        return Err(format!("unexpected row length {stride} bytes"));
    }
    Ok(FbInfo {
        width,
        height,
        stride,
    })
}

/// The bytes of one frame in the framebuffer's layout: each row is the pixels as little-endian
/// `XRGB8888`, then padding up to the stride.
pub fn frame_bytes(canvas: &Canvas, info: &FbInfo) -> Vec<u8> {
    let mut out = vec![0u8; info.stride * info.height];
    for y in 0..info.height.min(canvas.height) {
        let row = &mut out[y * info.stride..(y + 1) * info.stride];
        for x in 0..info.width.min(canvas.width) {
            let p = canvas
                .pixels
                .get(y * canvas.width + x)
                .copied()
                .unwrap_or(0);
            row[x * 4..x * 4 + 4].copy_from_slice(&[
                (p & 0xFF) as u8,
                ((p >> 8) & 0xFF) as u8,
                ((p >> 16) & 0xFF) as u8,
                0,
            ]);
        }
    }
    out
}

/// Somewhere to show a finished picture.
pub trait Display {
    /// Width and height in pixels.
    fn size(&self) -> (usize, usize);
    /// Shows the picture.
    ///
    /// # Errors
    /// The error from the device.
    fn present(&mut self, canvas: &Canvas) -> io::Result<()>;
}

/// The Linux framebuffer.
pub struct Framebuffer<W: Write + Seek> {
    out: W,
    info: FbInfo,
}

impl<W: Write + Seek> Framebuffer<W> {
    /// A framebuffer with this geometry, written through `out`.
    pub fn new(out: W, info: FbInfo) -> Self {
        Self { out, info }
    }
}

impl Framebuffer<File> {
    /// Opens the device and reads its geometry from `sys`.
    ///
    /// # Errors
    /// A message when the device or its description is missing or unsuitable.
    pub fn open(device: &Path, sys: &Path) -> Result<Self, String> {
        let read = |name: &str| {
            std::fs::read_to_string(sys.join(name))
                .map_err(|e| format!("no framebuffer ({}: {e})", sys.join(name).display()))
        };
        let info = parse_fb_info(
            &read("virtual_size")?,
            &read("stride")?,
            &read("bits_per_pixel")?,
        )?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(device)
            .map_err(|e| format!("cannot open {}: {e}", device.display()))?;
        Ok(Self::new(file, info))
    }
}

impl<W: Write + Seek> Display for Framebuffer<W> {
    fn size(&self) -> (usize, usize) {
        (self.info.width, self.info.height)
    }

    fn present(&mut self, canvas: &Canvas) -> io::Result<()> {
        let bytes = frame_bytes(canvas, &self.info);
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&bytes)?;
        self.out.flush()
    }
}

// ---------------------------------------------------------------- keyboard

const EV_KEY: u16 = 1;

/// Printable keys of a US keyboard by evdev code: `(code, plain, shifted)`.
const PRINTABLE: &[(u16, char, char)] = &[
    (2, '1', '!'),
    (3, '2', '@'),
    (4, '3', '#'),
    (5, '4', '$'),
    (6, '5', '%'),
    (7, '6', '^'),
    (8, '7', '&'),
    (9, '8', '*'),
    (10, '9', '('),
    (11, '0', ')'),
    (12, '-', '_'),
    (13, '=', '+'),
    (26, '[', '{'),
    (27, ']', '}'),
    (39, ';', ':'),
    (40, '\'', '"'),
    (41, '`', '~'),
    (43, '\\', '|'),
    (51, ',', '<'),
    (52, '.', '>'),
    (53, '/', '?'),
    (57, ' ', ' '),
];

/// The letter on a key, by evdev code.
fn letter(code: u16) -> Option<char> {
    const ROWS: [(u16, &str); 3] = [(16, "qwertyuiop"), (30, "asdfghjkl"), (44, "zxcvbnm")];
    ROWS.iter().find_map(|(first, row)| {
        let i = usize::from(code.checked_sub(*first)?);
        row.chars().nth(i)
    })
}

/// Turns evdev key records into [`Key`]s for a US layout, tracking Shift, Ctrl and Caps Lock.
#[derive(Debug, Clone, Default)]
pub struct KeyDecoder {
    buf: Vec<u8>,
    shift: bool,
    ctrl: bool,
    caps: bool,
}

impl KeyDecoder {
    /// A decoder with no keys held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes (in any split) and returns the keys they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buf.len() >= EV_SIZE {
            let rec: Vec<u8> = self.buf.drain(..EV_SIZE).collect();
            let kind = u16::from_le_bytes([rec[16], rec[17]]);
            let code = u16::from_le_bytes([rec[18], rec[19]]);
            let value = i32::from_le_bytes([rec[20], rec[21], rec[22], rec[23]]);
            if kind == EV_KEY && (0..=2).contains(&value) {
                if let Some(k) = self.key(code, value) {
                    out.push(k);
                }
            }
        }
        out
    }

    fn key(&mut self, code: u16, value: i32) -> Option<Key> {
        match code {
            42 | 54 => {
                self.shift = value != 0;
                return None;
            }
            29 | 97 => {
                self.ctrl = value != 0;
                return None;
            }
            58 => {
                if value == 1 {
                    self.caps = !self.caps;
                }
                return None;
            }
            _ => {}
        }
        if value == 0 {
            return None;
        }
        if let Some(c) = letter(code) {
            return Some(if self.ctrl {
                Key::Ctrl(c)
            } else if self.shift ^ self.caps {
                Key::Char(c.to_ascii_uppercase())
            } else {
                Key::Char(c)
            });
        }
        if let Some((_, plain, shifted)) = PRINTABLE.iter().find(|(c, _, _)| *c == code) {
            return Some(Key::Char(if self.shift { *shifted } else { *plain }));
        }
        Some(match code {
            1 => Key::Esc,
            14 => Key::Backspace,
            15 if self.shift => Key::BackTab,
            15 => Key::Tab,
            28 | 96 => Key::Enter,
            102 => Key::Home,
            103 => Key::Up,
            104 => Key::PageUp,
            105 => Key::Left,
            106 => Key::Right,
            107 => Key::End,
            108 => Key::Down,
            109 => Key::PageDown,
            111 => Key::Delete,
            59..=68 => Key::F(u8::try_from(code - 58).ok()?),
            87 => Key::F(11),
            88 => Key::F(12),
            _ => return None,
        })
    }
}

// ----------------------------------------------------------------- events

/// Something that happened.
#[derive(Debug)]
pub enum Ev {
    /// An input device appeared after the desktop started (the message names it).
    NewInput(String),
    /// The system area of the menu bar has new content (clock, network, Bluetooth, battery).
    Status(Box<SystemStatus>),
    /// The mouse did something.
    Mouse(MouseEvent),
    /// A key was pressed.
    Key(Key),
    /// The model produced a piece of its answer (shown at once, before the answer is complete).
    Piece(String),
    /// The model finished answering (or failed).
    Reply(Result<String, String>),
    /// A Wi-Fi scan finished (or could not run).
    WifiScanned(Result<Vec<mouli_screen::screens::WifiRow>, String>),
    /// Joining a Wi-Fi network finished: a summary of the link, or why not.
    WifiJoined(Result<String, String>),
    /// The system asked the desktop to stop (SIGTERM or SIGHUP).
    Terminate,
    /// The browser finished loading an address (or failed).
    Page {
        /// The address that was asked for.
        requested: String,
        /// The page, or why it could not be loaded.
        result: Result<crate::browse::Loaded, String>,
    },
}

/// What an input device can be used for, from the kernel's capability words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Roles {
    /// Letter keys: a keyboard.
    pub keyboard: bool,
    /// Relative X and Y: a mouse or a pointing stick.
    pub mouse: bool,
    /// Absolute X and Y with finger touch, not a touchscreen: a touchpad.
    pub touchpad: bool,
}

impl Roles {
    /// Whether the device is of any use here.
    pub fn any(self) -> bool {
        self.keyboard || self.mouse || self.touchpad
    }
}

/// Whether bit `n` is set in a sysfs capability string such as `"10000 0 0"` (hexadecimal words
/// of 64 bits, the most significant first).
pub fn cap_bit(words: &str, n: usize) -> bool {
    let all: Vec<&str> = words.split_whitespace().collect();
    let Some(word) = all.len().checked_sub(1 + n / 64).and_then(|i| all.get(i)) else {
        return false;
    };
    u64::from_str_radix(word, 16).is_ok_and(|w| (w >> (n % 64)) & 1 == 1)
}

/// Works out what a device is from its `rel`, `abs`, `key` and `properties` capability strings.
pub fn classify(rel: &str, abs: &str, key: &str, props: &str) -> Roles {
    const KEY_ENTER: usize = 28;
    const KEY_A: usize = 30;
    const BTN_TOUCH: usize = 0x14a;
    const INPUT_PROP_DIRECT: usize = 1;
    let has_xy =
        |w: &str| (cap_bit(w, 0) && cap_bit(w, 1)) || (cap_bit(w, 0x35) && cap_bit(w, 0x36));
    Roles {
        keyboard: cap_bit(key, KEY_A) && cap_bit(key, KEY_ENTER),
        mouse: cap_bit(rel, 0) && cap_bit(rel, 1),
        touchpad: has_xy(abs) && cap_bit(key, BTN_TOUCH) && !cap_bit(props, INPUT_PROP_DIRECT),
    }
}

/// The input devices found, for the boot message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputSummary {
    /// Keyboards.
    pub keyboards: Vec<String>,
    /// Mice and pointing sticks.
    pub mice: Vec<String>,
    /// Touchpads.
    pub touchpads: Vec<String>,
}

impl InputSummary {
    /// Whether anything can move the pointer.
    pub fn has_pointer(&self) -> bool {
        !self.mice.is_empty() || !self.touchpads.is_empty()
    }

    /// One line for the console.
    pub fn line(&self) -> String {
        let part = |label: &str, v: &[String]| {
            if v.is_empty() {
                format!("{label}: none")
            } else {
                format!("{label}: {}", v.join(", "))
            }
        };
        format!(
            "MOULI input: {}; {}; {}",
            part("keyboard", &self.keyboards),
            part("mouse", &self.mice),
            part("touchpad", &self.touchpads)
        )
    }
}

/// Opens every `event*` device in `dir` that is a keyboard, mouse or touchpad (judged from `sys`,
/// the kernel's `/sys/class/input`) and forwards what it reports on `tx`. A device whose
/// capabilities cannot be read is treated as a keyboard and a mouse. Touchpad movement is scaled
/// to a screen `screen_w` pixels wide. Each device has its own reader thread, which ends when the
/// device goes away or the receiver is dropped.
pub fn spawn_inputs(dir: &Path, sys: &Path, screen_w: i32, tx: &Sender<Ev>) -> InputSummary {
    scan_inputs(dir, sys, screen_w, tx, &mut BTreeSet::new())
}

/// Like [`spawn_inputs`], but skips the nodes in `seen` and adds the ones it opens to it, so it
/// can be called again to find devices that appear later (a USB or Bluetooth mouse).
pub fn scan_inputs(
    dir: &Path,
    sys: &Path,
    screen_w: i32,
    tx: &Sender<Ev>,
    seen: &mut BTreeSet<String>,
) -> InputSummary {
    let mut summary = InputSummary::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return summary;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let node = entry.file_name().to_string_lossy().into_owned();
        if !node.starts_with("event") || seen.contains(&node) {
            continue;
        }
        let base = sys.join(&node).join("device");
        let read = |rel: &str| std::fs::read_to_string(base.join(rel)).ok();
        let name = read("name")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| node.clone());
        let roles = match (read("capabilities/key"), read("capabilities/rel")) {
            (None, None) => Roles {
                keyboard: true,
                mouse: true,
                touchpad: false,
            },
            (key, rel) => classify(
                rel.as_deref().unwrap_or(""),
                &read("capabilities/abs").unwrap_or_default(),
                key.as_deref().unwrap_or(""),
                &read("properties").unwrap_or_default(),
            ),
        };
        if !roles.any() {
            continue;
        }
        let Ok(mut file) = File::open(entry.path()) else {
            continue;
        };
        seen.insert(node.clone());
        if roles.keyboard {
            summary.keyboards.push(name.clone());
        }
        if roles.mouse {
            summary.mice.push(name.clone());
        }
        if roles.touchpad {
            summary.touchpads.push(name.clone());
        }
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut mouse = EvdevDecoder::new(EV_SIZE);
            let mut pad = TouchpadDecoder::new(EV_SIZE, screen_w);
            let mut keys = KeyDecoder::new();
            let mut buf = [0u8; EV_SIZE * 32];
            while let Ok(n) = file.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut moved = Vec::new();
                if roles.mouse {
                    moved.extend(mouse.feed(&buf[..n]));
                }
                if roles.touchpad {
                    moved.extend(pad.feed(&buf[..n]));
                }
                for m in moved {
                    if tx.send(Ev::Mouse(m)).is_err() {
                        return;
                    }
                }
                if roles.keyboard {
                    for k in keys.feed(&buf[..n]) {
                        if tx.send(Ev::Key(k)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
    summary
}

/// How often the desktop looks for newly connected input devices.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Starts a thread that looks for input devices that appear later and reports each on `tx`. It
/// stops when the receiver is gone.
pub fn watch_inputs(
    dir: std::path::PathBuf,
    sys: std::path::PathBuf,
    screen_w: i32,
    tx: Sender<Ev>,
    mut seen: BTreeSet<String>,
) {
    std::thread::spawn(move || loop {
        std::thread::sleep(WATCH_EVERY);
        let found = scan_inputs(&dir, &sys, screen_w, &tx, &mut seen);
        let names: Vec<&String> = found
            .keyboards
            .iter()
            .chain(&found.mice)
            .chain(&found.touchpads)
            .collect();
        if !names.is_empty() {
            let list: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            if tx
                .send(Ev::NewInput(format!(
                    "New input device: {}",
                    list.join(", ")
                )))
                .is_err()
            {
                return;
            }
        }
    });
}

/// Starts a thread that gathers the system status every `tick` and sends it on `tx` whenever it
/// changed. `offset_min` is the UTC offset in minutes, shared with the desktop so that
/// `/time.zone` takes effect at once. The thread stops when the receiver is gone.
pub fn spawn_status<R: Runner + Send + 'static>(
    mut runner: R,
    env: SysEnv,
    tx: Sender<Ev>,
    offset_min: Arc<AtomicI32>,
    tick: Duration,
) {
    std::thread::spawn(move || {
        let mut collector = Collector::new(env);
        let mut last: Option<SystemStatus> = None;
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            let s = collector.status(&mut runner, now, offset_min.load(Ordering::Relaxed));
            if last.as_ref() != Some(&s) {
                if tx.send(Ev::Status(Box::new(s.clone()))).is_err() {
                    return;
                }
                last = Some(s);
            }
            std::thread::sleep(tick);
        }
    });
}

/// Handles `/time.zone`: shows or sets the UTC offset used by the clock. Returns the notice to show.
fn time_zone_command(
    arg: &str,
    offset: &AtomicI32,
    store: Option<&crate::persist::Store>,
) -> String {
    let arg = arg.trim();
    if arg.is_empty() {
        return format!(
            "The clock shows {}. To change it, type /time.zone +05:30 (or -8, or UTC).",
            sysinfo::zone_label(offset.load(Ordering::Relaxed))
        );
    }
    let Some(minutes) = sysinfo::parse_offset(arg) else {
        return "That is not a time zone. Examples: /time.zone +05:30, /time.zone -8, /time.zone UTC (from -12:00 to +14:00).".into();
    };
    offset.store(minutes, Ordering::Relaxed);
    let saved = match store {
        Some(s) => match s.set(sysinfo::OFFSET_KEY, &minutes.to_string()) {
            Ok(()) => " It is kept for the next start.",
            Err(_) => " (It could not be saved.)",
        },
        None => " (No MOULI-DATA disk, so it is forgotten at power-off.)",
    };
    format!("Time zone set to {}.{saved}", sysinfo::zone_label(minutes))
}

/// How the desktop is shown: stronger colours, and bigger text for small or far screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewOpts {
    /// Whether the high-contrast colours are on.
    contrast: bool,
    /// How many screen pixels each desktop pixel covers (1 to 3).
    scale: usize,
}

const CONTRAST_KEY: &str = "view.contrast";
const SCALE_KEY: &str = "view.scale";
const MAX_SCALE: usize = 3;

/// The saved view options (defaults when nothing is saved or a value is not understood).
fn load_view(store: Option<&crate::persist::Store>) -> ViewOpts {
    let saved = store
        .map(crate::persist::Store::settings)
        .unwrap_or_default();
    ViewOpts {
        contrast: saved.get(CONTRAST_KEY).is_some_and(|v| v == "on"),
        scale: saved
            .get(SCALE_KEY)
            .and_then(|v| v.parse().ok())
            .filter(|k| (1..=MAX_SCALE).contains(k))
            .unwrap_or(1),
    }
}

/// The scale that can really be used on a `w` by `h` screen: the desktop needs at least 640 by
/// 480 of its own pixels.
fn usable_scale(want: usize, w: usize, h: usize) -> usize {
    (1..=want.clamp(1, MAX_SCALE))
        .rev()
        .find(|k| w / k >= 640 && h / k >= 480)
        .unwrap_or(1)
}

/// The picture as it goes to the screen.
fn shape(canvas: mouli_screen::raster::Canvas, v: ViewOpts) -> mouli_screen::raster::Canvas {
    let c = if v.contrast {
        canvas.high_contrast()
    } else {
        canvas
    };
    c.scaled(v.scale)
}

fn save_view(store: Option<&crate::persist::Store>, key: &str, value: &str) -> &'static str {
    match store {
        Some(s) => match s.set(key, value) {
            Ok(()) => " It is kept for the next start.",
            Err(_) => " (It could not be saved.)",
        },
        None => " (No MOULI-DATA disk, so it is forgotten at power-off.)",
    }
}

/// Blanks the screen after a while without a key or the mouse, and wakes it on the next one.
/// (There is no lock: MOULI has no password to ask for yet, so waking needs no proof.)
#[derive(Debug, Clone, Copy)]
struct IdleBlank {
    timeout: Option<Duration>,
    last_input: Instant,
    blanked: bool,
}

impl IdleBlank {
    fn new(minutes: u32, now: Instant) -> Self {
        Self {
            timeout: (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60)),
            last_input: now,
            blanked: false,
        }
    }

    /// How long to wait for input before blanking; `None` when blanking is off or done.
    fn wait(&self, now: Instant) -> Option<Duration> {
        if self.blanked {
            return None;
        }
        self.timeout
            .map(|t| t.saturating_sub(now.saturating_duration_since(self.last_input)))
    }

    /// True (once) when it is time to blank the screen.
    fn due(&mut self, now: Instant) -> bool {
        if self.wait(now) == Some(Duration::ZERO) {
            self.blanked = true;
            return true;
        }
        false
    }

    /// A key or the mouse. Returns true when it only woke the screen and must not act.
    fn input(&mut self, now: Instant) -> bool {
        self.last_input = now;
        std::mem::replace(&mut self.blanked, false)
    }
}

const IDLE_KEY: &str = "view.idle";
const DEFAULT_IDLE_MIN: u32 = 10;

fn load_idle(store: Option<&crate::persist::Store>) -> u32 {
    store
        .and_then(|s| s.settings().get(IDLE_KEY).cloned())
        .and_then(|v| v.parse().ok())
        .filter(|m| [0, 5, 10, 30].contains(m))
        .unwrap_or(DEFAULT_IDLE_MIN)
}

fn idle_label(minutes: u32) -> String {
    if minutes == 0 {
        "off".into()
    } else {
        format!("{minutes} min")
    }
}

/// Writes the picture as a new PNG file `screenshot-N.png` in `dir` (created if needed) and
/// returns its name. Never replaces an existing file.
fn save_screenshot(canvas: &Canvas, dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let png = mouli_screen::raster::to_png(canvas);
    for n in 1..10_000 {
        let name = format!("screenshot-{n}.png");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))
        {
            Ok(mut f) => {
                return std::io::Write::write_all(&mut f, &png)
                    .and_then(|()| f.sync_all())
                    .map(|()| name)
                    .map_err(|e| e.to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("too many screenshots".into())
}

/// Signal strength as 1 to 4 bars.
fn wifi_bars(dbm: i32) -> u8 {
    match dbm {
        s if s >= -55 => 4,
        s if s >= -67 => 3,
        s if s >= -78 => 2,
        _ => 1,
    }
}

/// Looks for networks on the first wireless adapter and returns them for the Wi-Fi screen.
fn wifi_scan_rows<R: Runner>(
    r: &mut R,
    sys_net: &Path,
) -> Result<Vec<mouli_screen::screens::WifiRow>, String> {
    use crate::wifi::Security;
    use mouli_screen::screens::{WifiRow, WifiSecurity};
    let ifaces = crate::wifi::wireless_interfaces(sys_net);
    let iface = ifaces.first().ok_or(
        "No Wi-Fi adapter was found. Use a network cable, or an adapter the kernel knows.",
    )?;
    Ok(crate::wifi::scan(r, iface, 3)?
        .into_iter()
        .map(|n| WifiRow {
            bars: wifi_bars(n.signal),
            security: match n.security {
                Security::Open => WifiSecurity::Open,
                Security::Personal => WifiSecurity::Password,
                Security::Unsupported => WifiSecurity::Unsupported,
            },
            ssid: n.ssid,
        })
        .collect())
}

/// Joins `ssid` on the first adapter, gets an address, and remembers the network on the data
/// disk. Returns the one-line summary of the link.
fn wifi_join_network(
    sys_net: &Path,
    save_to: Option<&Path>,
    ssid: &str,
    passphrase: Option<&str>,
) -> Result<String, String> {
    let ifaces = crate::wifi::wireless_interfaces(sys_net);
    let iface = ifaces.first().ok_or("No Wi-Fi adapter was found.")?;
    let link = crate::wifi::connect(&mut crate::wifi::System, iface, ssid, passphrase)?;
    let mut text = link.summary();
    if let Some(root) = save_to {
        match crate::wifi_saved::remember(root, ssid, passphrase, unix_now()) {
            Ok(()) => text.push_str("  Saved: MOULI will join this network next time."),
            Err(e) => text.push_str(&format!("  (could not save this network: {e})")),
        }
    }
    Ok(text)
}

/// Scans on its own thread so the desktop stays alive (a scan takes seconds).
fn spawn_wifi_scan(tx: &Sender<Ev>) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = wifi_scan_rows(&mut crate::wifi::System, Path::new(crate::net::SYS_NET));
        let _ = tx.send(Ev::WifiScanned(result));
    });
}

/// The rows of the Settings screen from what the desktop is running with.
fn settings_rows(
    view: &ViewOpts,
    wanted_scale: usize,
    models: &ModelsInfo,
    offset: &AtomicI32,
    user: &str,
    idle_min: u32,
) -> Vec<mouli_screen::screens::SettingRow> {
    use mouli_screen::screens::SettingRow;
    let row = |id: &str, label: &str, value: String, choices: &[&str], note: &str| SettingRow {
        id: id.into(),
        label: label.into(),
        value,
        choices: choices.iter().map(|c| (*c).to_string()).collect(),
        note: note.into(),
    };
    vec![
        row(
            "contrast",
            "High contrast",
            if view.contrast { "on" } else { "off" }.into(),
            &["off", "on"],
            "Stronger colours for hard-to-see screens. Takes effect at once.",
        ),
        row(
            "textsize",
            "Text size",
            format!("{wanted_scale}x"),
            &["1x", "2x", "3x"],
            "Larger text for small or far screens. Takes effect after a restart.",
        ),
        row(
            "idle",
            "Blank screen after",
            idle_label(idle_min),
            &["off", "5 min", "10 min", "30 min"],
            "Blanks after no key or mouse use; any key wakes it (no lock yet: there is no password).",
        ),
        row(
            "memory",
            "Memory for a model",
            format!("{} MB", models.budget_mb),
            &[],
            "Chosen from this machine's memory when it starts.",
        ),
        row(
            "context",
            "Context length",
            format!("{} tokens", models.ctx),
            &[],
            "Chosen with the memory above; longer needs more memory.",
        ),
        row(
            "timezone",
            "Time zone",
            sysinfo::zone_label(offset.load(Ordering::Relaxed)),
            &[],
            "Type /time.zone +05:30 (or -8, or UTC) in Chat to change it.",
        ),
        row(
            "user",
            "Signed in as",
            user.to_string(),
            &[],
            "Type /signin <name> <role> in Chat to switch.",
        ),
    ]
}

/// Handles `/contrast [on|off]`. Returns the notice to show.
fn contrast_command(
    arg: &str,
    view: &mut ViewOpts,
    store: Option<&crate::persist::Store>,
) -> String {
    match arg.trim() {
        "" => format!(
            "High contrast is {}. Type /contrast on or /contrast off.",
            if view.contrast { "on" } else { "off" }
        ),
        a @ ("on" | "off") => {
            view.contrast = a == "on";
            let saved = save_view(store, CONTRAST_KEY, a);
            format!("High contrast {a}.{saved}")
        }
        _ => "Type /contrast on or /contrast off.".into(),
    }
}

/// Handles `/textsize [1|2|3]`. The size applies from the next start. Returns the notice.
fn text_size_command(
    arg: &str,
    view: &ViewOpts,
    screen: (usize, usize),
    store: Option<&crate::persist::Store>,
) -> String {
    let arg = arg.trim();
    if arg.is_empty() {
        return format!(
            "Text size is {}x (in use: {}x). Type /textsize 1, 2 or 3; it applies after a restart.",
            view.scale,
            usable_scale(view.scale, screen.0, screen.1)
        );
    }
    let Some(k) = arg
        .parse::<usize>()
        .ok()
        .filter(|k| (1..=MAX_SCALE).contains(k))
    else {
        return "Type /textsize 1, 2 or 3.".into();
    };
    let usable = usable_scale(k, screen.0, screen.1);
    let saved = save_view(store, SCALE_KEY, &k.to_string());
    let limit = if usable < k {
        format!(" This screen is too small for {k}x; {usable}x will be used.")
    } else {
        String::new()
    };
    format!("Text size {k}x applies after a restart.{limit}{saved}")
}

// ------------------------------------------------------------------ console

/// Keeps typed characters from being echoed onto the framebuffer console while the desktop is
/// up, and puts the terminal back afterwards.
#[cfg(target_os = "linux")]
struct TtyGuard {
    saved: Option<nix::sys::termios::Termios>,
}

/// The terminal settings to put back, kept where a panic hook can reach them.
#[cfg(target_os = "linux")]
static SAVED_TTY: std::sync::Mutex<Option<nix::sys::termios::Termios>> =
    std::sync::Mutex::new(None);

/// How many times the console was restored (the guard, or the panic hook).
#[cfg(target_os = "linux")]
static RESTORES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Puts the terminal back as it was and shows the cursor. Safe to call more than once and from
/// any thread; does nothing to settings it never saved.
#[cfg(target_os = "linux")]
pub fn restore_console() {
    use nix::sys::termios::{tcflush, tcsetattr, FlushArg, SetArg};
    RESTORES.fetch_add(1, Ordering::SeqCst);
    let stdin = std::io::stdin();
    let _ = tcflush(&stdin, FlushArg::TCIFLUSH);
    let saved = SAVED_TTY.lock().map(|g| g.clone()).unwrap_or(None);
    if let Some(t) = &saved {
        let _ = tcsetattr(&stdin, SetArg::TCSANOW, t);
    }
    let _ = write!(std::io::stdout(), "\x1b[?25h\x1b[2J\x1b[H");
    let _ = std::io::stdout().flush();
}

/// Makes a crash put the console back before the panic message is printed, so the message can be
/// read and the keyboard still echoes. (The release build aborts on panic, which skips every
/// destructor; this hook is what runs instead.)
#[cfg(target_os = "linux")]
pub fn restore_console_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_console();
        previous(info);
    }));
}

#[cfg(target_os = "linux")]
impl TtyGuard {
    fn new() -> Self {
        use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
        let stdin = std::io::stdin();
        let saved = tcgetattr(&stdin).ok();
        if let Ok(mut slot) = SAVED_TTY.lock() {
            slot.clone_from(&saved);
        }
        if let Some(t) = &saved {
            let mut raw = t.clone();
            raw.local_flags
                .remove(LocalFlags::ECHO | LocalFlags::ICANON);
            let _ = tcsetattr(&stdin, SetArg::TCSANOW, &raw);
        }
        // Hide the text cursor and clear the console.
        let _ = write!(std::io::stdout(), "\x1b[?25l\x1b[2J\x1b[H");
        let _ = std::io::stdout().flush();
        Self { saved }
    }
}

#[cfg(target_os = "linux")]
impl Drop for TtyGuard {
    fn drop(&mut self) {
        let _ = &self.saved;
        restore_console();
    }
}

#[cfg(not(target_os = "linux"))]
struct TtyGuard;

#[cfg(not(target_os = "linux"))]
impl TtyGuard {
    fn new() -> Self {
        Self
    }
}

// ------------------------------------------------------------------- the loop

/// The device's own model route is not used: the desktop talks to the model on its own thread
/// (so the screen stays alive while it thinks) and keeps the conversation itself.
pub struct NoRoute;

impl DeviceBackend for NoRoute {
    fn complete(&mut self, _: &Route, _: &str) -> Result<String, String> {
        Err("chat is handled by the desktop".into())
    }
}

/// The person using the desktop and what they may do. One owner on this machine.
const TENANT: &str = "local";
const USER: &str = "owner";

/// Builds the governed workspace with the Code application (with its agent approvals), and the
/// device that connects the screens to it.
///
/// Sheet, Studio, Player and the CRM are left out of this version on purpose; their code is kept
/// for later versions (see `MOVED_OUT` in the desktop).
///
/// With `data_dir` (the MOULI-DATA partition) the Code window opens `<data_dir>/projects`.
///
/// # Errors
/// A message when the workspace cannot be set up.
pub fn build_device(data_dir: Option<&Path>) -> Result<Device<NoRoute>, String> {
    let fail = |e: &dyn std::fmt::Display| format!("cannot set up the applications: {e}");
    let mut ws = Workspace::new(Policy::default(), Default::default()).map_err(|e| fail(&e))?;
    mouli_device::add_default_roles(&mut ws);
    // What the model may do when it uses tools: the Code tools only. Changes still need approval.
    ws.add_role(Permissions::new("agent", &["code.*"]));
    ws.add_model(ModelEntry {
        id: "local".into(),
        provider: "local".into(),
        local: true,
        caps: BTreeSet::from(["text".to_string()]),
        context: 4096,
        cost_per_1k: 0,
        quality: 50,
    });
    let code = mouli_apps::register_code(&mut ws).map_err(|e| fail(&e))?;
    // Saved audit chain and held approvals come back first. A damaged file never stops the boot
    // and is never overwritten: the device then runs without saving and says so.
    let state = data_dir.map(|d| d.join("state"));
    let restored = state.as_deref().map(|d| mouli_device::restore(&mut ws, d));
    let mut device = Device::new(ws, NoRoute, TENANT, USER, "owner").map_err(|e| fail(&e))?;
    match (&state, restored) {
        (Some(dir), Some(Ok(_))) => {
            if let Err(e) = device.keep_state_in(dir) {
                device.shell.approvals.message = format!("Audit log is not being saved: {e}");
            }
            // With accounts set up (or an accounts file that cannot be read) nobody is signed in
            // until they give a passphrase.
            device.lock();
        }
        (_, Some(Err(e))) => {
            device.shell.approvals.message =
                format!("Saved audit log not loaded, and not being saved: {e}");
        }
        _ => {}
    }
    device.attach_code(code);
    if let Some(dir) = data_dir {
        device.set_export_dir(&dir.join("exports"));
        let projects = dir.join("projects");
        std::fs::create_dir_all(&projects).map_err(|e| fail(&e))?;
        // A project that cannot be opened leaves the Code window empty; that is not fatal.
        let _ = device.open_project(&projects);
    }
    Ok(device)
}

/// What [`start`] prepared: a screen, the input stream and the applications.
pub struct Session<D: Display> {
    display: D,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    device: Device<NoRoute>,
    saving: bool,
    inputs: InputSummary,
    offset: Arc<AtomicI32>,
    settings: Option<crate::persist::Store>,
    graph: Recorder,
    data_root: Option<std::path::PathBuf>,
}

/// Seconds since the Unix epoch (0 if the clock is before it).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl<D: Display> Session<D> {
    /// A session over any display and event channel (used by tests).
    pub fn new(display: D, tx: Sender<Ev>, rx: Receiver<Ev>, device: Device<NoRoute>) -> Self {
        Self {
            display,
            tx,
            rx,
            device,
            saving: false,
            inputs: InputSummary::default(),
            offset: Arc::new(AtomicI32::new(0)),
            settings: None,
            graph: Recorder::in_memory(unix_now()),
            data_root: None,
        }
    }

    /// The input devices found, for the boot message.
    pub fn inputs(&self) -> &InputSummary {
        &self.inputs
    }
}

/// Opens the framebuffer, the input devices and the applications.
///
/// # Errors
/// A message saying why the desktop cannot start; the text chat should be used instead.
pub fn start(data_dir: Option<&Path>) -> Result<Session<Framebuffer<File>>, String> {
    let display = Framebuffer::open(Path::new(FB_DEVICE), Path::new(FB_SYS))?;
    let device = build_device(data_dir)?;
    let (tx, rx) = channel();
    // A crash puts the console back first; SIGTERM and SIGHUP end the desktop the normal way.
    #[cfg(target_os = "linux")]
    {
        restore_console_on_panic();
        let stop = tx.clone();
        if let Err(e) = crate::shutdown::on_stop_signal(move || {
            let _ = stop.send(Ev::Terminate);
        }) {
            eprintln!("moulid: cannot watch for SIGTERM: {e}");
        }
    }
    let width = i32::try_from(display.size().0).unwrap_or(1024);
    let mut seen = BTreeSet::new();
    let inputs = scan_inputs(
        Path::new(INPUT_DIR),
        Path::new(INPUT_SYS),
        width,
        &tx,
        &mut seen,
    );
    if inputs.keyboards.is_empty() && !inputs.has_pointer() {
        return Err("no keyboard, mouse or touchpad found in /dev/input".into());
    }
    watch_inputs(INPUT_DIR.into(), INPUT_SYS.into(), width, tx.clone(), seen);
    let mut s = Session::new(display, tx.clone(), rx, device);
    s.saving = data_dir.is_some();
    s.inputs = inputs;
    s.settings = data_dir.and_then(|d| crate::persist::Store::open(d).ok());
    if let Some(d) = data_dir {
        s.graph = Recorder::open(d, unix_now());
        s.data_root = Some(d.to_path_buf());
    }
    if let Some(m) = s
        .settings
        .as_ref()
        .and_then(|st| st.settings().get(sysinfo::OFFSET_KEY).cloned())
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|m| (sysinfo::MIN_OFFSET..=sysinfo::MAX_OFFSET).contains(m))
    {
        s.offset.store(m, Ordering::Relaxed);
    }
    spawn_status(
        crate::wifi::System,
        SysEnv::real(),
        tx,
        s.offset.clone(),
        Duration::from_secs(1),
    );
    Ok(s)
}

/// The models the Models window shows, and which one is loaded.
#[derive(Debug, Clone, Default)]
pub struct ModelsInfo {
    /// Every model found on the machine.
    pub rows: Vec<ModelRow>,
    /// Memory a model may use, in MB.
    pub budget_mb: u64,
    /// The planned context length.
    pub ctx: u32,
    /// The name of the model that is loaded.
    pub loaded: String,
}

impl ModelsInfo {
    /// A list with only the loaded model (when the others are not known).
    #[must_use]
    pub fn only(name: &str) -> Self {
        Self {
            rows: vec![ModelRow {
                name: name.into(),
                size_mb: 0,
                arch: None,
                needs_mb: 0,
                fits: true,
            }],
            budget_mb: 0,
            ctx: 0,
            loaded: name.into(),
        }
    }
}

/// How fast an answer arrives, from its streamed pieces.
///
/// Time is counted from the first piece to the latest, so the wait before the model starts (reading
/// the question) is not in the rate. A piece is usually one token but not always (a hosted service
/// may send several at once), so the figure is shown as approximate.
#[derive(Debug, Default)]
struct Speed {
    first: Option<Instant>,
    last: Option<Instant>,
    pieces: usize,
    /// The rate of the last finished answer.
    finished: Option<f64>,
}

impl Speed {
    /// Shortest time the rate is worked out over; a shorter stretch gives a meaningless figure.
    const MIN_SECS: f64 = 0.25;

    /// A piece arrived at `now`.
    fn piece(&mut self, now: Instant) {
        if self.first.is_none() || self.last.is_none() {
            self.first = Some(now);
            self.pieces = 0;
        }
        self.last = Some(now);
        self.pieces += 1;
    }

    /// The rate so far (or of the answer just finished): pieces after the first, per second.
    fn rate(&self) -> Option<f64> {
        let (first, last) = (self.first?, self.last?);
        let secs = last.saturating_duration_since(first).as_secs_f64();
        (self.pieces >= 2 && secs >= Self::MIN_SECS).then(|| (self.pieces - 1) as f64 / secs)
    }

    /// The answer ended: keep its rate for the status line and start afresh.
    fn finish(&mut self) {
        if let Some(r) = self.rate() {
            self.finished = Some(r);
        }
        self.first = None;
        self.last = None;
        self.pieces = 0;
    }

    /// Text for the status line: the live rate while an answer is arriving, else the last one.
    fn label(&self) -> Option<String> {
        self.rate()
            .or(self.finished)
            .map(|r| format!("~{r:.1} tok/s"))
    }
}

/// The text at the right of the menu bar: the product name, who is signed in, the model, how fast
/// the last answer arrived, and a reminder when requests wait for a second person's approval (the
/// Approvals window is not on the desktop).
fn status_text(base: &str, user: &str, model: &str, speed: Option<&str>, pending: usize) -> String {
    let mut t = format!("{base}  |  {user}");
    if !model.is_empty() {
        t.push_str(&format!("  |  {model}"));
    }
    if let Some(s) = speed {
        t.push_str(&format!("  |  {s}"));
    }
    match pending {
        0 => {}
        1 => t.push_str("  |  1 waiting for approval (F3)"),
        n => t.push_str(&format!("  |  {n} waiting for approval (F3)")),
    }
    t
}

/// Fills the Models window and the chat title from `info`.
fn show_models(shell: &mut Shell, info: &ModelsInfo) {
    shell.models.rows = info.rows.clone();
    shell.models.budget_mb = info.budget_mb;
    shell.models.ctx = info.ctx;
    if let Some(i) = info.rows.iter().position(|r| r.name == info.loaded) {
        shell.models.cursor.selected = i;
    }
    shell.models.message = format!(
        "Loaded: {}. To switch, restart and pick another.",
        info.loaded
    );
    shell.chat.model = info.loaded.clone();
}

fn fresh(system_prompt: &str) -> Vec<Turn> {
    vec![Turn {
        role: "system",
        content: system_prompt.into(),
    }]
}

/// Runs the desktop until the person shuts down.
///
/// The Chat window talks to the model on its own thread, so the desktop stays responsive while it
/// thinks. Code, Approvals, Audit and Security run through the governed
/// workspace of the device.
///
/// # Errors
/// An error from drawing to the display.
pub fn run<B, D>(
    session: Session<D>,
    backend: B,
    system_prompt: &str,
    max_tokens: usize,
    models: &ModelsInfo,
) -> io::Result<End>
where
    B: Backend + Send + 'static,
    D: Display,
{
    let Session {
        mut display,
        tx,
        rx,
        mut device,
        saving,
        inputs,
        offset,
        settings,
        mut graph,
        data_root,
    } = session;
    let _tty = TtyGuard::new();
    let (w, h) = display.size();
    let mut view = load_view(settings.as_ref());
    let mut idle_min = load_idle(settings.as_ref());
    let mut idle = IdleBlank::new(idle_min, Instant::now());
    let mut wanted_scale = view.scale;
    let scale = usable_scale(view.scale, w, h);
    view.scale = scale;
    let mut desktop = Desktop::new(
        i32::try_from(w / scale).unwrap_or(1024),
        i32::try_from(h / scale).unwrap_or(768),
    );
    let base_status = if saving {
        "MOULI OS".to_string()
    } else {
        "MOULI OS - nothing is saved (no MOULI-DATA disk)".to_string()
    };
    desktop.status = base_status.clone();
    show_models(&mut device.shell, models);
    device.shell.settings.set_rows(settings_rows(
        &view,
        wanted_scale,
        models,
        &offset,
        device.user(),
        idle_min,
    ));
    desktop.open(&mut device.shell, Tab::Chat);
    if !inputs.has_pointer() {
        device.shell.chat.push(
            Role::Notice,
            "No mouse or touchpad found. Plug in a USB mouse, or use the keyboard: F1 opens Chat, F2 Models, F9 Code, Ctrl+Q shuts down.",
        );
    }
    device.shell.chat.push(Role::Notice, &inputs.line());
    let commands: Vec<String> = device
        .workspace()
        .help()
        .iter()
        .map(|(name, _)| format!("/{name}"))
        .take(14)
        .collect();
    if !commands.is_empty() {
        device.shell.chat.push(
            Role::Notice,
            &format!(
                "Commands you can type here (the apps run them through the governed gate): {} ...",
                commands.join(", ")
            ),
        );
    }
    let tool_names: Vec<String> = device
        .workspace()
        .help()
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    let system_text = {
        let extra = toolloop::tool_prompt(&device.workspace().help());
        if extra.is_empty() {
            system_prompt.to_string()
        } else {
            format!("{system_prompt}\n\n{extra}")
        }
    };
    let mut steps = 0usize;
    if let Some(w) = graph.warning.take() {
        device.shell.chat.push(Role::Notice, &w);
    }
    let model_name = models.loaded.clone();
    refresh_storage(&mut device.shell, data_root.as_deref());
    let mut history = fresh(&system_text);

    let (req_tx, req_rx) = channel::<Vec<Turn>>();
    let reply_tx = tx.clone();
    let reply_tx_pages = tx.clone();
    // Ctrl-C while the model is answering sets this; the answer thread then stops reading.
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = cancel.clone();
    let piece_tx = tx.clone();
    std::thread::spawn(move || {
        let mut backend = backend;
        while let Ok(turns) = req_rx.recv() {
            let answer = backend.reply_streaming(&turns, &mut |piece| {
                piece_tx.send(Ev::Piece(piece.to_string())).is_ok()
                    && !thread_cancel.load(Ordering::SeqCst)
            });
            let answer = match answer {
                Ok(text) if thread_cancel.load(Ordering::SeqCst) => {
                    Ok(format!("{text}\n[stopped]"))
                }
                other => other,
            };
            if reply_tx.send(Ev::Reply(answer)).is_err() {
                break;
            }
        }
    });

    let mut speed = Speed::default();
    let started = Instant::now();
    let now_ms = move || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    display.present(&shape(desktop.render(&device.shell), view))?;
    loop {
        let mut batch = Vec::new();
        if device.shell.preview.wants_ticks() || device.shell.browser.view.wants_ticks() {
            // A page with timers: wake up often enough to run them.
            match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(first) => batch.push(first),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let a = device.shell.preview.tick();
                    let b = device.shell.browser.view.tick();
                    if (a || b) && !idle.blanked {
                        display.present(&shape(desktop.render(&device.shell), view))?;
                    }
                    continue;
                }
                Err(_) => return Ok(End::Eof),
            }
        } else if let Some(wait) = idle.wait(Instant::now()) {
            match rx.recv_timeout(wait) {
                Ok(first) => batch.push(first),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if idle.due(Instant::now()) {
                        let (bw, bh) = display.size();
                        display.present(&Canvas {
                            width: bw,
                            height: bh,
                            pixels: vec![0; bw * bh],
                        })?;
                    }
                    continue;
                }
                Err(_) => return Ok(End::Eof),
            }
        } else {
            let Ok(first) = rx.recv() else {
                return Ok(End::Eof);
            };
            batch.push(first);
        }
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
            if batch.len() >= 256 {
                break;
            }
        }
        for ev in batch {
            if matches!(ev, Ev::Key(_) | Ev::Mouse(_)) && idle.input(Instant::now()) {
                continue; // this key or movement only woke the screen
            }
            let action = match ev {
                Ev::Mouse(m) => desktop.on_mouse(&mut device.shell, m, now_ms()),
                Ev::Key(Key::Ctrl('p')) => {
                    let note = match &data_root {
                        Some(root) => {
                            let picture = shape(desktop.render(&device.shell), view);
                            match save_screenshot(&picture, &root.join("exports")) {
                                Ok(name) => {
                                    device.record_event("screenshot.save", &name, "ok");
                                    format!("Screenshot saved as exports/{name}")
                                }
                                Err(e) => format!("Screenshot not saved: {e}"),
                            }
                        }
                        None => "Screenshot not saved: there is no data disk.".into(),
                    };
                    device.shell.chat.push(Role::Notice, &note);
                    Action::None
                }
                Ev::Key(Key::Ctrl('c')) if device.shell.chat.busy => {
                    cancel.store(true, Ordering::SeqCst);
                    Action::None
                }
                Ev::Key(k) => desktop.on_key(&mut device.shell, k),
                Ev::Piece(piece) => {
                    speed.piece(Instant::now());
                    device.shell.chat.stream_piece(&piece);
                    Action::None
                }
                Ev::Reply(r) => {
                    speed.finish();
                    let answer = on_reply(&mut device.shell, &mut history, r);
                    // A stopped answer is shown but never run as a tool call.
                    let stopped = cancel.swap(false, Ordering::SeqCst);
                    let answer = answer.filter(|_| !stopped);
                    if let Some(text) = answer {
                        graph.assistant_message(&text, &model_name, unix_now());
                        let known: Vec<&str> = tool_names.iter().map(String::as_str).collect();
                        if let Some(command) = toolloop::parse_tool_call(&text, &known) {
                            if steps >= toolloop::MAX_STEPS {
                                device.shell.chat.push(
                                    Role::Notice,
                                    "The model asked for more tools than allowed for one question; stopped.",
                                );
                            } else {
                                steps += 1;
                                let held_before: Vec<String> = device
                                    .shell
                                    .approvals
                                    .rows
                                    .iter()
                                    .map(|r| r.id.clone())
                                    .collect();
                                let output = device.run_agent(&command);
                                let now = unix_now();
                                graph.tool_call(mouli_device::AGENT_USER, &command, &output, now);
                                for row in &device.shell.approvals.rows {
                                    if !held_before.contains(&row.id) {
                                        graph.approval_held(&row.id, &row.capability, now);
                                    }
                                }
                                history.push(Turn {
                                    role: "user",
                                    content: toolloop::result_turn(&command, &output),
                                });
                                trim_history(&mut history, max_tokens);
                                device.shell.chat.busy = true;
                                if req_tx.send(history.clone()).is_err() {
                                    device.shell.chat.busy = false;
                                }
                            }
                        }
                    }
                    Action::None
                }
                Ev::NewInput(text) => {
                    device.shell.chat.push(Role::Notice, &text);
                    Action::None
                }
                Ev::Status(s) => {
                    desktop.system = *s;
                    Action::None
                }
                Ev::Terminate => return Ok(End::Quit),
                Ev::WifiScanned(result) => {
                    device.shell.wifi.scanned(result);
                    Action::None
                }
                Ev::WifiJoined(result) => {
                    device.shell.wifi.joined(result);
                    Action::None
                }
                Ev::Page { requested, result } => {
                    match result {
                        Ok(l) => device
                            .shell
                            .browser
                            .loaded_with(&requested, &l.url, l.html, l.images),
                        Err(m) => {
                            let page = crate::browse::error_page(&requested, &m);
                            device.shell.browser.failed(&requested, &m, page);
                        }
                    }
                    Action::None
                }
            };
            match action {
                Action::None => {}
                Action::Show(tab) => {
                    desktop.open(&mut device.shell, tab);
                    if tab == Tab::Storage {
                        refresh_storage(&mut device.shell, data_root.as_deref());
                    }
                    if tab == Tab::Wifi && device.shell.wifi.wants_first_scan() {
                        spawn_wifi_scan(&tx);
                    }
                }
                Action::Wifi => {
                    desktop.open(&mut device.shell, Tab::Wifi);
                    if device.shell.wifi.wants_first_scan() {
                        spawn_wifi_scan(&tx);
                    }
                }
                Action::WifiScan => spawn_wifi_scan(&tx),
                Action::WifiJoin { ssid, passphrase } => {
                    let tx = tx.clone();
                    let save_to = data_root.clone();
                    std::thread::spawn(move || {
                        let result = wifi_join_network(
                            Path::new(crate::net::SYS_NET),
                            save_to.as_deref(),
                            &ssid,
                            passphrase
                                .as_ref()
                                .map(mouli_screen::screen::Hidden::expose),
                        );
                        let _ = tx.send(Ev::WifiJoined(result));
                    });
                }
                Action::Navigate(url) => {
                    let tx = reply_tx_pages.clone();
                    std::thread::spawn(move || {
                        let result = match crate::fetch::HttpFetcher::system() {
                            Ok(f) => crate::browse::load(&f, &url),
                            Err(e) => Err(e),
                        };
                        let _ = tx.send(Ev::Page {
                            requested: url,
                            result,
                        });
                    });
                }
                Action::RescanStorage => {
                    refresh_storage(&mut device.shell, data_root.as_deref());
                    device.shell.storage.message = "Looked at the disk again.".into();
                }
                Action::DeleteStorage(i) => {
                    let message = delete_stored_model(
                        &mut device.shell,
                        data_root.as_deref(),
                        i,
                        &model_name,
                        &mut graph,
                    );
                    refresh_storage(&mut device.shell, data_root.as_deref());
                    device.shell.storage.message = message;
                }
                Action::Send(text) if text.trim_start().starts_with("/time.zone") => {
                    device.shell.chat.push(Role::User, &text);
                    let arg = text.trim_start()["/time.zone".len()..].to_string();
                    let note = time_zone_command(&arg, &offset, settings.as_ref());
                    device.shell.chat.push(Role::Notice, &note);
                }
                Action::Setting { id, value } => {
                    let note = match id.as_str() {
                        "contrast" => contrast_command(&value, &mut view, settings.as_ref()),
                        "textsize" => {
                            let k = value.trim_end_matches('x').to_string();
                            if let Ok(n) = k.parse::<usize>() {
                                wanted_scale = n;
                            }
                            let (sw, sh) = display.size();
                            text_size_command(&k, &view, (sw, sh), settings.as_ref())
                        }
                        "idle" => {
                            let m = value.trim_end_matches(" min");
                            match (m == "off", m.parse::<u32>()) {
                                (true, _) | (_, Ok(0 | 5 | 10 | 30)) => {
                                    idle_min = m.parse().unwrap_or(0);
                                    idle = IdleBlank::new(idle_min, Instant::now());
                                    let saved = save_view(
                                        settings.as_ref(),
                                        IDLE_KEY,
                                        &idle_min.to_string(),
                                    );
                                    format!("Screen blanking: {}.{saved}", idle_label(idle_min))
                                }
                                _ => "Choose off, 5, 10 or 30 minutes.".into(),
                            }
                        }
                        _ => "That setting cannot be changed here.".into(),
                    };
                    device.shell.settings.message = note;
                }
                Action::Send(text) if text.trim_start().starts_with("/contrast") => {
                    device.shell.chat.push(Role::User, &text);
                    let arg = text.trim_start()["/contrast".len()..].to_string();
                    let note = contrast_command(&arg, &mut view, settings.as_ref());
                    device.shell.chat.push(Role::Notice, &note);
                }
                Action::Send(text) if text.trim_start().starts_with("/textsize") => {
                    device.shell.chat.push(Role::User, &text);
                    let arg = text.trim_start()["/textsize".len()..].to_string();
                    let (sw, sh) = display.size();
                    if let Some(k) = arg
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .filter(|k| (1..=MAX_SCALE).contains(k))
                    {
                        wanted_scale = k;
                    }
                    let note = text_size_command(&arg, &view, (sw, sh), settings.as_ref());
                    device.shell.chat.push(Role::Notice, &note);
                }
                Action::Send(text) if text.trim_start().starts_with("/graph") => {
                    device.shell.chat.push(Role::User, &text);
                    let arg = text.trim_start()["/graph".len()..].trim().to_string();
                    let note = if let Some(path) = arg.strip_prefix(".file") {
                        graph.describe_file(path.trim())
                    } else if arg == ".forget" {
                        graph.forget(unix_now())
                    } else if arg.is_empty() {
                        graph.describe_recent()
                    } else {
                        "Use /graph (recent activity), /graph.file PATH (history of a file) or /graph.forget (erase the record).".to_string()
                    };
                    device.shell.chat.push(Role::Notice, &note);
                }
                Action::Send(text) if text.trim_start().starts_with('/') => {
                    // A command for an application: carried out through the governed workspace
                    // (policy, approvals, audit), not sent to the model.
                    let before = device.shell.chat.messages.len();
                    device.run(Action::Send(text.clone()));
                    let output = device.shell.chat.messages[before..]
                        .iter()
                        .filter(|m| m.role != Role::User)
                        .map(|m| m.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    graph.tool_call("owner", &text, &output, unix_now());
                }
                Action::Send(text) => {
                    // A page summary asked for in the browser is answered in the Chat window.
                    let from_browser = text.starts_with("Summarise this web page");
                    if from_browser {
                        if device.shell.chat.busy {
                            device.shell.browser.status =
                                "The assistant is busy; try again in a moment.".into();
                        } else {
                            desktop.open(&mut device.shell, Tab::Chat);
                        }
                    }
                    if !device.shell.chat.busy {
                        device.shell.chat.push(Role::User, &text);
                        graph.user_message(&text, unix_now());
                        steps = 0;
                        history.push(Turn {
                            role: "user",
                            content: text,
                        });
                        trim_history(&mut history, max_tokens);
                        device.shell.chat.busy = true;
                        if req_tx.send(history.clone()).is_err() {
                            device.shell.chat.busy = false;
                            device
                                .shell
                                .chat
                                .push(Role::Notice, "The model is not running.");
                        }
                    }
                }
                Action::NewConversation => {
                    if !device.shell.chat.busy {
                        device.run(Action::NewConversation);
                        history = fresh(&system_text);
                        steps = 0;
                        graph.new_conversation(unix_now());
                    }
                }
                other => {
                    let decision = if let Action::Decide { id, approve } = &other {
                        Some((id.clone(), *approve))
                    } else {
                        None
                    };
                    let outcome = device.run(other);
                    if let Some((id, approve)) = decision {
                        if !device.shell.approvals.rows.iter().any(|r| r.id == id) {
                            graph.approval_decided(&id, approve, "owner", unix_now());
                        }
                    }
                    match outcome {
                    Outcome::Quit => return Ok(End::Quit),
                    Outcome::Reboot => return Ok(End::Reboot),
                    Outcome::Handled => {}
                    Outcome::Host(_) => device.shell.chat.push(
                        Role::Notice,
                        "That is not available in the desktop yet; restart with mouli.text=1 for the text menu.",
                    ),
                    }
                }
            }
        }
        desktop.status = status_text(
            &base_status,
            device.user(),
            &device.shell.chat.model,
            speed.label().as_deref(),
            device.shell.approvals.rows.len(),
        );
        device.shell.settings.set_rows(settings_rows(
            &view,
            wanted_scale,
            models,
            &offset,
            device.user(),
            idle_min,
        ));
        if !idle.blanked {
            display.present(&shape(desktop.render(&device.shell), view))?;
        }
    }
}

/// Fills the Storage window from the data disk (or shows that there is none).
fn refresh_storage(shell: &mut Shell, root: Option<&Path>) {
    match root {
        Some(r) => {
            let s = crate::storage::scan(r);
            shell.storage.set(s.rows, s.free, s.total, "MOULI-DATA");
        }
        None => shell.storage.set(Vec::new(), None, None, ""),
    }
}

/// Removes the model the person chose in the Storage window, unless it is the one in use.
/// Returns the message to show.
fn delete_stored_model(
    shell: &mut Shell,
    root: Option<&Path>,
    index: usize,
    loaded: &str,
    graph: &mut Recorder,
) -> String {
    let Some(root) = root else {
        return "There is no data disk.".into();
    };
    let Some(row) = shell.storage.rows.get(index).cloned() else {
        return "That row is not on the list any more.".into();
    };
    let stem = row
        .name
        .rsplit_once('.')
        .map_or(row.name.as_str(), |(s, _)| s);
    if !loaded.is_empty() && (row.name == loaded || stem == loaded) {
        return format!("{} is the model in use; load another one first.", row.name);
    }
    match crate::storage::remove_model(root, &row) {
        crate::storage::Removal::Removed(name, bytes) => {
            graph.model_removed(&name, unix_now());
            format!(
                "Removed {name} and freed {}.",
                mouli_screen::screens::size_text(bytes)
            )
        }
        crate::storage::Removal::Refused(why) => why,
    }
}

/// Records the model's reply; returns its text when it answered.
fn on_reply(
    shell: &mut Shell,
    history: &mut Vec<Turn>,
    reply: Result<String, String>,
) -> Option<String> {
    shell.chat.busy = false;
    match reply {
        Ok(text) => {
            shell.chat.finish_answer(&text);
            history.push(Turn {
                role: "assistant",
                content: text.clone(),
            });
            Some(text)
        }
        Err(e) => {
            shell.chat.streaming = false;
            if history.last().is_some_and(|t| t.role == "user") {
                history.pop();
            }
            shell
                .chat
                .push(Role::Notice, &format!("The model could not answer: {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rec(kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut v = vec![0u8; 16];
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&code.to_le_bytes());
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    fn press(code: u16) -> Vec<u8> {
        [rec(1, code, 1), rec(1, code, 0)].concat()
    }

    #[test]
    fn fb_info_is_parsed_and_checked() {
        let i = parse_fb_info("1440,900\n", "5760\n", "32\n").unwrap();
        assert_eq!((i.width, i.height, i.stride), (1440, 900, 5760));
        assert!(parse_fb_info("1440x900", "5760", "32").is_err());
        assert!(parse_fb_info("1440,900", "5760", "16").is_err());
        assert!(parse_fb_info("320,200", "1280", "32").is_err(), "too small");
        assert!(
            parse_fb_info("99999,900", "400000", "32").is_err(),
            "too large"
        );
        assert!(
            parse_fb_info("1440,900", "100", "32").is_err(),
            "row shorter than the width"
        );
        assert!(parse_fb_info("a,b", "c", "32").is_err());
        assert!(parse_fb_info("", "", "").is_err());
    }

    #[test]
    fn frame_bytes_respects_stride_and_colour_order() {
        let info = FbInfo {
            width: 640,
            height: 480,
            stride: 640 * 4 + 64,
        };
        let mut c = Canvas {
            width: 640,
            height: 480,
            pixels: vec![0; 640 * 480],
        };
        c.pixels[0] = 0x0011_2233; // red 0x11, green 0x22, blue 0x33
        c.pixels[640] = 0x00AA_BBCC; // first pixel of row 1
        let b = frame_bytes(&c, &info);
        assert_eq!(b.len(), info.stride * 480);
        assert_eq!(&b[0..4], &[0x33, 0x22, 0x11, 0]);
        assert_eq!(&b[info.stride..info.stride + 4], &[0xCC, 0xBB, 0xAA, 0]);
        assert!(
            b[640 * 4..info.stride].iter().all(|&x| x == 0),
            "padding stays empty"
        );
    }

    #[test]
    fn a_smaller_canvas_does_not_panic() {
        let info = FbInfo {
            width: 640,
            height: 480,
            stride: 2560,
        };
        let c = Canvas {
            width: 10,
            height: 10,
            pixels: vec![1; 100],
        };
        assert_eq!(frame_bytes(&c, &info).len(), 2560 * 480);
    }

    #[test]
    fn framebuffer_writes_a_whole_frame_from_the_start() {
        let info = FbInfo {
            width: 640,
            height: 480,
            stride: 2560,
        };
        let mut fb = Framebuffer::new(Cursor::new(vec![9u8; 10]), info);
        let c = Canvas {
            width: 640,
            height: 480,
            pixels: vec![0x00FF_FFFF; 640 * 480],
        };
        fb.present(&c).unwrap();
        fb.present(&c).unwrap();
        assert_eq!(fb.size(), (640, 480));
        let out = fb.out.into_inner();
        assert_eq!(out.len(), 2560 * 480);
        assert_eq!(&out[0..4], &[255, 255, 255, 0]);
    }

    #[test]
    fn letters_digits_and_symbols() {
        let mut d = KeyDecoder::new();
        assert_eq!(d.feed(&press(16)), vec![Key::Char('q')]);
        assert_eq!(d.feed(&press(30)), vec![Key::Char('a')]);
        assert_eq!(d.feed(&press(50)), vec![Key::Char('m')]);
        assert_eq!(d.feed(&press(2)), vec![Key::Char('1')]);
        assert_eq!(d.feed(&press(11)), vec![Key::Char('0')]);
        assert_eq!(d.feed(&press(57)), vec![Key::Char(' ')]);
        assert_eq!(d.feed(&press(53)), vec![Key::Char('/')]);
    }

    #[test]
    fn every_letter_key_gives_its_own_letter() {
        let mut d = KeyDecoder::new();
        let mut got = String::new();
        for code in (16..=25).chain(30..=38).chain(44..=50) {
            for k in d.feed(&press(code)) {
                if let Key::Char(c) = k {
                    got.push(c);
                }
            }
        }
        assert_eq!(got, "qwertyuiopasdfghjklzxcvbnm");
    }

    #[test]
    fn shift_caps_and_ctrl() {
        let mut d = KeyDecoder::new();
        let shifted = [rec(1, 42, 1), rec(1, 30, 1), rec(1, 30, 0), rec(1, 42, 0)].concat();
        assert_eq!(d.feed(&shifted), vec![Key::Char('A')]);
        assert_eq!(
            d.feed(&press(30)),
            vec![Key::Char('a')],
            "shift is released"
        );
        let digits = [rec(1, 54, 1), rec(1, 2, 1), rec(1, 2, 0), rec(1, 54, 0)].concat();
        assert_eq!(d.feed(&digits), vec![Key::Char('!')]);
        d.feed(&press(58)); // caps lock on
        assert_eq!(d.feed(&press(30)), vec![Key::Char('A')]);
        d.feed(&press(58)); // off
        assert_eq!(d.feed(&press(30)), vec![Key::Char('a')]);
        let ctrl = [rec(1, 29, 1), rec(1, 17, 1), rec(1, 17, 0), rec(1, 29, 0)].concat();
        assert_eq!(d.feed(&ctrl), vec![Key::Ctrl('w')]);
        let back = [rec(1, 42, 1), rec(1, 15, 1), rec(1, 15, 0), rec(1, 42, 0)].concat();
        assert_eq!(d.feed(&back), vec![Key::BackTab]);
    }

    #[test]
    fn special_and_function_keys() {
        let mut d = KeyDecoder::new();
        for (code, key) in [
            (28, Key::Enter),
            (1, Key::Esc),
            (14, Key::Backspace),
            (15, Key::Tab),
            (111, Key::Delete),
            (103, Key::Up),
            (108, Key::Down),
            (105, Key::Left),
            (106, Key::Right),
            (102, Key::Home),
            (107, Key::End),
            (104, Key::PageUp),
            (109, Key::PageDown),
            (59, Key::F(1)),
            (68, Key::F(10)),
            (87, Key::F(11)),
            (88, Key::F(12)),
        ] {
            assert_eq!(d.feed(&press(code)), vec![key], "code {code}");
        }
    }

    #[test]
    fn repeats_count_releases_and_unknown_keys_do_not() {
        let mut d = KeyDecoder::new();
        assert_eq!(d.feed(&rec(1, 30, 2)), vec![Key::Char('a')], "auto-repeat");
        assert!(d.feed(&rec(1, 30, 0)).is_empty(), "release");
        assert!(d.feed(&press(116)).is_empty(), "power key is ignored");
        assert!(d.feed(&rec(0, 0, 0)).is_empty(), "sync record");
        assert!(d.feed(&rec(1, 30, 7)).is_empty(), "odd value");
    }

    #[test]
    fn records_split_across_reads_are_joined() {
        let mut d = KeyDecoder::new();
        let bytes = press(30);
        let (a, b) = bytes.split_at(10);
        assert!(d.feed(a).is_empty());
        assert_eq!(d.feed(b), vec![Key::Char('a')]);
        assert!(d.feed(&[1, 2, 3]).is_empty(), "a partial record waits");
    }

    #[test]
    fn missing_input_directory_means_no_devices() {
        let (tx, _rx) = channel();
        let none = spawn_inputs(
            Path::new("/nonexistent-mouli-input"),
            Path::new("/nonexistent-sys"),
            1280,
            &tx,
        );
        assert_eq!(none, InputSummary::default());
    }

    #[test]
    fn input_devices_are_read_and_decoded() {
        let dir = std::env::temp_dir().join(format!("moulid-input-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut data = press(30);
        data.extend(rec(2, 0, 5)); // REL_X +5
        data.extend(rec(0, 0, 0)); // SYN
        std::fs::write(dir.join("event0"), &data).unwrap();
        std::fs::write(dir.join("mouse0"), b"ignored").unwrap();
        let (tx, rx) = channel();
        let found = spawn_inputs(&dir, Path::new("/nonexistent-sys"), 1280, &tx);
        assert_eq!(found.keyboards.len(), 1, "only event* files are read");
        let mut keys = 0;
        let mut moves = 0;
        for _ in 0..2 {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                Ev::Key(Key::Char('a')) => keys += 1,
                Ev::Mouse(MouseEvent::Move { dx: 5, dy: 0 }) => moves += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!((keys, moves), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct Echo;
    impl Backend for Echo {
        fn reply(&mut self, history: &[Turn]) -> Result<String, String> {
            let last = history
                .last()
                .map(|t| t.content.clone())
                .unwrap_or_default();
            if last == "fail" {
                Err("no model".into())
            } else {
                Ok(format!("echo: {last}"))
            }
        }
    }

    struct Shots(std::sync::Arc<std::sync::Mutex<Vec<Canvas>>>);
    impl Display for Shots {
        fn size(&self) -> (usize, usize) {
            (800, 600)
        }
        fn present(&mut self, c: &Canvas) -> io::Result<()> {
            self.0.lock().unwrap().push(c.clone());
            Ok(())
        }
    }

    fn keys_for(text: &str) -> Vec<Ev> {
        let mut v = Vec::new();
        for ch in text.chars() {
            v.push(Ev::Key(if ch == '\n' {
                Key::Enter
            } else {
                Key::Char(ch)
            }));
        }
        v
    }

    #[test]
    fn typing_a_message_gets_an_answer_and_ctrl_q_ends() {
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let session = Session::new(
            Shots(shots.clone()),
            tx.clone(),
            rx,
            build_device(None).unwrap(),
        );
        for e in keys_for("hi\n") {
            tx.send(e).unwrap();
        }
        let t2 = tx.clone();
        let seen = shots.clone();
        let quitter = std::thread::spawn(move || {
            // Wait until the reply has been drawn (start, keys, reply), then shut down.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while seen.lock().unwrap().len() < 3 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        let end = run(session, Echo, "be nice", 10_000, &ModelsInfo::only("tiny")).unwrap();
        quitter.join().unwrap();
        assert_eq!(end, End::Quit);
        let n = shots.lock().unwrap().len();
        assert!(
            n >= 3,
            "drawn at start, after the keys and after the reply: {n}"
        );
    }

    #[test]
    fn replies_update_the_history_and_failures_are_shown() {
        let mut shell = Shell::new();
        let mut h = fresh("sys");
        h.push(Turn {
            role: "user",
            content: "q".into(),
        });
        shell.chat.busy = true;
        on_reply(&mut shell, &mut h, Ok("a".into()));
        assert!(!shell.chat.busy);
        assert_eq!(h.last().unwrap().role, "assistant");
        h.push(Turn {
            role: "user",
            content: "fail".into(),
        });
        shell.chat.busy = true;
        on_reply(&mut shell, &mut h, Err("boom".into()));
        assert!(!shell.chat.busy);
        assert_eq!(
            h.last().unwrap().role,
            "assistant",
            "the failed question is dropped"
        );
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moulid-gui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_device_is_built_with_and_without_a_data_disk() {
        assert!(build_device(None).is_ok());
        let d = tmp("dev");
        std::fs::create_dir_all(d.join("projects")).unwrap();
        std::fs::write(d.join("projects/hello.txt"), "hi\n").unwrap();
        assert!(build_device(Some(&d)).is_ok());
        assert!(d.join("projects").is_dir());
        // A data disk that cannot be written is an error, not a panic.
        assert!(build_device(Some(Path::new("/proc/mouli-nonexistent"))).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_audit_chain_is_kept_on_the_data_disk_and_a_damaged_one_is_not_overwritten() {
        let d = tmp("audit-kept");
        {
            let device = build_device(Some(&d)).unwrap();
            assert!(!device.workspace().audit().records().is_empty());
        }
        let first = std::fs::read_to_string(d.join("state/audit.log")).unwrap();
        let again = build_device(Some(&d)).unwrap();
        assert!(again.workspace().audit().records().len() > first.lines().count());
        assert_eq!(again.workspace().audit().verify(), Ok(()));
        std::fs::write(d.join("state/audit.log"), "junk\n").unwrap();
        let broken = build_device(Some(&d)).unwrap();
        assert!(broken.shell.approvals.message.contains("not loaded"));
        assert_eq!(
            std::fs::read_to_string(d.join("state/audit.log")).unwrap(),
            "junk\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_moved_out_applications_do_not_open() {
        let dir = tmp("movedout");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let session = Session::new(Shots(shots), tx.clone(), rx, device);
        for n in [7u8, 8, 10] {
            tx.send(Ev::Key(Key::F(n))).unwrap();
        }
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        let end = run(session, Echo, "sys", 1000, &ModelsInfo::only("tiny")).unwrap();
        assert_eq!(end, End::Quit);
        assert!(!dir.join("apps").exists(), "no sheet or document is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capability_bits_are_read_from_the_sysfs_words() {
        assert!(cap_bit("10000 0 0", 128 + 16));
        assert!(!cap_bit("10000 0 0", 16));
        assert!(cap_bit("3", 0) && cap_bit("3", 1) && !cap_bit("3", 2));
        assert!(!cap_bit("", 0));
        assert!(!cap_bit("zz", 0));
        assert!(!cap_bit("3", 5000), "a bit beyond the words is clear");
    }

    #[test]
    fn devices_are_classified_by_what_they_can_do() {
        let letters = "ffffffffffffffff";
        // A keyboard: letter and Enter keys.
        let kb = classify("", "", letters, "0");
        assert!(kb.keyboard && !kb.mouse && !kb.touchpad);
        // A mouse: relative X and Y, and a few buttons (BTN_LEFT is bit 0x110 = 272).
        let mouse = classify("143", "", "1f0000 0 0 0 0", "0");
        assert!(mouse.mouse && !mouse.keyboard && !mouse.touchpad);
        // A touchpad: absolute X and Y, and BTN_TOUCH (bit 0x14a = 330, word 5 from the right).
        let pad = classify("", "3", "400 0 0 0 0 0", "5");
        assert!(pad.touchpad && !pad.mouse);
        // A touchscreen has the DIRECT property and is left alone.
        let screen = classify("", "3", "400 0 0 0 0 0", "2");
        assert!(!screen.touchpad);
        // Multitouch-only pads (MT position axes 0x35 and 0x36 = 53, 54).
        let mt = classify("", "60000000000000", "400 0 0 0 0 0", "1");
        assert!(mt.touchpad);
        // Power and lid switches are not input for us.
        assert!(!classify("", "", "0", "0").any());
    }

    #[test]
    fn the_input_summary_names_what_was_found() {
        let mut s = InputSummary::default();
        assert!(!s.has_pointer());
        assert!(s.line().contains("touchpad: none"));
        s.keyboards.push("AT keyboard".into());
        s.touchpads.push("Apple trackpad".into());
        assert!(s.has_pointer());
        assert_eq!(
            s.line(),
            "MOULI input: keyboard: AT keyboard; mouse: none; touchpad: Apple trackpad"
        );
    }

    #[test]
    fn devices_are_opened_only_when_the_kernel_says_they_are_input_we_use() {
        let root = tmp("sysdev");
        let (dev, sys) = (root.join("dev"), root.join("sys"));
        for (node, name, key, rel, abs) in [
            ("event0", "Test keyboard", "ffffffffffffffff", "0", "0"),
            ("event1", "Test mouse", "1f0000 0 0 0 0", "143", "0"),
            ("event2", "Power Button", "0", "0", "0"),
            ("event3", "Test pad", "400 0 0 0 0 0", "0", "3"),
        ] {
            std::fs::create_dir_all(dev.clone()).unwrap();
            std::fs::write(dev.join(node), b"").unwrap();
            let caps = sys.join(node).join("device/capabilities");
            std::fs::create_dir_all(&caps).unwrap();
            std::fs::write(sys.join(node).join("device/name"), format!("{name}\n")).unwrap();
            std::fs::write(sys.join(node).join("device/properties"), "5\n").unwrap();
            for (f, v) in [("key", key), ("rel", rel), ("abs", abs)] {
                std::fs::write(caps.join(f), format!("{v}\n")).unwrap();
            }
        }
        let (tx, _rx) = channel();
        let s = spawn_inputs(&dev, &sys, 1280, &tx);
        assert_eq!(s.keyboards, vec!["Test keyboard"]);
        assert_eq!(s.mice, vec!["Test mouse"]);
        assert_eq!(s.touchpads, vec!["Test pad"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_device_that_appears_later_is_found_once() {
        let root = tmp("later");
        let (dev, sys) = (root.join("dev"), root.join("sys"));
        std::fs::create_dir_all(&dev).unwrap();
        let (tx, _rx) = channel();
        let mut seen = BTreeSet::new();
        assert_eq!(
            scan_inputs(&dev, &sys, 1280, &tx, &mut seen),
            InputSummary::default()
        );
        // A mouse connects.
        std::fs::write(dev.join("event7"), b"").unwrap();
        let caps = sys.join("event7/device/capabilities");
        std::fs::create_dir_all(&caps).unwrap();
        std::fs::write(sys.join("event7/device/name"), "Late mouse\n").unwrap();
        for (f, v) in [("key", "1f0000 0 0 0 0"), ("rel", "143"), ("abs", "0")] {
            std::fs::write(caps.join(f), v).unwrap();
        }
        let first = scan_inputs(&dev, &sys, 1280, &tx, &mut seen);
        assert_eq!(first.mice, vec!["Late mouse"]);
        let again = scan_inputs(&dev, &sys, 1280, &tx, &mut seen);
        assert_eq!(again, InputSummary::default(), "not opened twice");
        let _ = std::fs::remove_dir_all(&root);
    }

    struct Counting(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Backend for Counting {
        fn reply(&mut self, _: &[Turn]) -> Result<String, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("model answer".into())
        }
    }

    #[test]
    fn the_models_window_lists_the_models_and_says_which_is_loaded() {
        let mut shell = Shell::new();
        let info = ModelsInfo {
            rows: vec![
                ModelRow {
                    name: "small".into(),
                    size_mb: 800,
                    arch: Some("llama".into()),
                    needs_mb: 1000,
                    fits: true,
                },
                ModelRow {
                    name: "big".into(),
                    size_mb: 4000,
                    arch: None,
                    needs_mb: 5000,
                    fits: false,
                },
            ],
            budget_mb: 3000,
            ctx: 2048,
            loaded: "big".into(),
        };
        show_models(&mut shell, &info);
        assert_eq!(shell.models.rows.len(), 2);
        assert_eq!(
            shell.models.selected().unwrap().name,
            "big",
            "the loaded model is selected"
        );
        assert_eq!((shell.models.budget_mb, shell.models.ctx), (3000, 2048));
        assert!(shell.models.message.contains("Loaded: big"));
        assert_eq!(shell.chat.model, "big");
        // With only a name known, there is still one row.
        show_models(&mut shell, &ModelsInfo::only("x"));
        assert_eq!(shell.models.rows.len(), 1);
    }

    #[test]
    fn a_slash_command_in_the_chat_runs_the_application_and_never_reaches_the_model() {
        let dir = tmp("slash");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let session = Session::new(Shots(shots), tx.clone(), rx, device);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for e in keys_for("/code.status\n") {
            tx.send(e).unwrap();
        }
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        let end = run(
            session,
            Counting(calls.clone()),
            "sys",
            1000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        assert_eq!(end, End::Quit);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the model was not asked"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_text_still_goes_to_the_model() {
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let session = Session::new(
            Shots(shots.clone()),
            tx.clone(),
            rx,
            build_device(None).unwrap(),
        );
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for e in keys_for("hello there\n") {
            tx.send(e).unwrap();
        }
        let t2 = tx.clone();
        let seen = shots.clone();
        let quitter = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while seen.lock().unwrap().len() < 3 && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        run(
            session,
            Counting(calls.clone()),
            "sys",
            1000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        quitter.join().unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn only_chat_models_and_code_are_registered_in_this_version() {
        let mut d = build_device(None).unwrap();
        let names: Vec<String> = d
            .workspace()
            .help()
            .iter()
            .map(|(n, _)| (*n).to_string())
            .collect();
        assert!(names.iter().any(|n| n == "code.status"), "{names:?}");
        for gone in ["crm.", "sheet.", "studio.", "player."] {
            assert!(
                !names.iter().any(|n| n.starts_with(gone)),
                "{gone} moved out: {names:?}"
            );
        }
        d.run(Action::Send("/crm.lead.list".into()));
        assert_eq!(
            d.shell.chat.messages.last().unwrap().role,
            Role::Notice,
            "an unknown command is explained, not sent to the model"
        );
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn speed_is_counted_from_the_first_piece_not_from_the_question() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        assert_eq!(s.label(), None);
        // One piece alone, or pieces within a few milliseconds, give no figure yet.
        s.piece(at(t0, 5_000));
        assert_eq!(s.label(), None);
        s.piece(at(t0, 5_050));
        assert_eq!(s.label(), None, "too short a stretch to mean anything");
        // 11 pieces (at 5.000, 5.050, then every 100 ms to 6.000): 10 intervals in one second. The 5 s wait before is not counted.
        for k in 2..=10u64 {
            s.piece(at(t0, 5_000 + k * 100));
        }
        assert_eq!(s.label().as_deref(), Some("~10.0 tok/s"));
    }

    #[test]
    fn a_finished_answer_keeps_its_rate_until_the_next_one_has_one() {
        let t0 = Instant::now();
        let mut s = Speed::default();
        for k in 0..=10u64 {
            s.piece(at(t0, k * 200)); // 5 per second
        }
        s.finish();
        assert_eq!(
            s.label().as_deref(),
            Some("~5.0 tok/s"),
            "kept after the answer"
        );
        // The next answer starts afresh and does not mix with the old one.
        s.piece(at(t0, 10_000));
        assert_eq!(
            s.label().as_deref(),
            Some("~5.0 tok/s"),
            "no new figure yet"
        );
        for k in 1..=10u64 {
            s.piece(at(t0, 10_000 + k * 50)); // 20 per second
        }
        assert_eq!(s.label().as_deref(), Some("~20.0 tok/s"));
        // An answer with too few pieces to measure leaves the last good figure alone.
        s.finish();
        s.piece(at(t0, 20_000));
        s.finish();
        assert_eq!(s.label().as_deref(), Some("~20.0 tok/s"));
    }

    #[test]
    fn the_status_line_shows_the_rate_between_the_model_and_the_approvals() {
        let t = status_text("MOULI OS", "amy", "tiny", Some("~7.5 tok/s"), 2);
        assert!(
            t.contains("amy  |  tiny  |  ~7.5 tok/s  |  2 waiting for approval (F3)"),
            "{t}"
        );
        assert!(!status_text("MOULI OS", "amy", "tiny", None, 0).contains("tok/s"));
    }

    #[test]
    fn the_menu_bar_reminds_when_approvals_are_waiting() {
        assert_eq!(
            status_text("MOULI OS", "alice", "", None, 0),
            "MOULI OS  |  alice"
        );
        assert_eq!(
            status_text("MOULI OS", "alice", "tiny", None, 0),
            "MOULI OS  |  alice  |  tiny"
        );
        assert!(status_text("MOULI OS", "a", "", None, 1).contains("1 waiting for approval (F3)"));
        assert!(status_text("MOULI OS", "a", "m", None, 3).contains("3 waiting for approval (F3)"));
    }

    struct NoRun;
    impl Runner for NoRun {
        fn run(&mut self, _: &str, _: &[&str]) -> Option<String> {
            None
        }
        fn pause(&mut self, _: Duration) {}
    }

    fn next_status(rx: &Receiver<Ev>) -> Box<SystemStatus> {
        loop {
            match rx
                .recv_timeout(Duration::from_secs(10))
                .expect("a status arrives")
            {
                Ev::Status(s) => return s,
                _ => continue,
            }
        }
    }

    #[test]
    fn the_status_thread_reports_the_clock_and_follows_a_new_zone() {
        let root = tmp("statusthread");
        let env = SysEnv {
            sys_net: root.join("net"),
            sys_bt: root.join("bt"),
            power: root.join("power"),
            dbus_socket: root.join("none"),
        };
        let (tx, rx) = channel();
        let offset = Arc::new(AtomicI32::new(0));
        spawn_status(NoRun, env, tx, offset.clone(), Duration::from_millis(10));
        let first = next_status(&rx);
        assert_eq!(first.zone, "UTC");
        assert_eq!(first.time.len(), 5, "HH:MM: {}", first.time);
        offset.store(330, Ordering::Relaxed);
        let mut zone = String::new();
        for _ in 0..200 {
            zone = next_status(&rx).zone;
            if zone == "UTC+05:30" {
                break;
            }
        }
        assert_eq!(zone, "UTC+05:30");
        // Unchanged status is not sent again and the thread ends when the receiver is gone.
        drop(rx);
        std::thread::sleep(Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn contrast_and_text_size_commands_validate_save_and_reload() {
        let dir = tmp("view");
        let store = crate::persist::Store::open(&dir).unwrap();
        let mut v = load_view(Some(&store));
        assert_eq!(
            v,
            ViewOpts {
                contrast: false,
                scale: 1
            }
        );
        assert!(contrast_command("", &mut v, Some(&store)).contains("off"));
        assert!(contrast_command("loud", &mut v, Some(&store)).contains("on or"));
        assert!(!v.contrast);
        assert!(contrast_command("on", &mut v, Some(&store)).contains("kept"));
        assert!(v.contrast);
        assert!(text_size_command("2", &v, (1024, 768), Some(&store)).contains("restart"));
        assert!(text_size_command("9", &v, (1024, 768), Some(&store)).contains("1, 2 or 3"));
        // Too small a screen for 3x is said plainly.
        assert!(text_size_command("3", &v, (1024, 768), Some(&store)).contains("too small"));
        assert!(text_size_command("2", &v, (1024, 768), None).contains("forgotten"));
        // 3x was saved last; it is read back, and what can really be used is clamped.
        let again = load_view(Some(&store));
        assert!(again.contrast);
        assert_eq!(again.scale, 3);
        assert_eq!(usable_scale(3, 1024, 768), 1);
        assert_eq!(usable_scale(2, 1280, 960), 2);
        assert_eq!(usable_scale(3, 1920, 1440), 3);
        assert_eq!(usable_scale(2, 1920, 1080), 2);
        assert_eq!(usable_scale(0, 1920, 1080), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_settings_screen_shows_the_real_values_and_changes_contrast() {
        let dir = tmp("settings-screen");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots.clone()), tx.clone(), rx, device);
        session.settings = Some(crate::persist::Store::open(&dir).unwrap());
        // ^O opens Settings; Enter changes the first row (High contrast) to on; ^Q quits.
        for e in [
            Ev::Key(Key::Ctrl('o')),
            Ev::Key(Key::Enter),
            Ev::Key(Key::Ctrl('q')),
        ] {
            tx.send(e).unwrap();
        }
        run(
            session,
            Scripted {
                replies: vec!["ok"],
                seen: Default::default(),
            },
            "base prompt",
            100_000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        let saved = crate::persist::Store::open(&dir).unwrap().settings();
        assert_eq!(saved.get(CONTRAST_KEY).map(String::as_str), Some("on"));
        // The last picture was drawn in high contrast: pure black and pure white are present.
        let last = shots.lock().unwrap().last().unwrap().clone();
        assert!(last.pixels.contains(&0x000000) && last.pixels.contains(&0xFFFFFF));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_settings_rows_report_what_is_running() {
        let off = AtomicI32::new(330);
        let m = ModelsInfo::only("tiny");
        let rows = settings_rows(
            &ViewOpts {
                contrast: true,
                scale: 1,
            },
            2,
            &m,
            &off,
            "owner",
            10,
        );
        let get = |id: &str| rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(get("contrast").value, "on");
        assert_eq!(get("textsize").value, "2x");
        assert_eq!(get("idle").value, "10 min");
        assert_eq!(get("idle").choices.len(), 4);
        assert!(get("memory").choices.is_empty());
        assert!(
            get("timezone").value.contains("5:30"),
            "{}",
            get("timezone").value
        );
        assert_eq!(get("user").value, "owner");
    }

    /// A supplicant that is already running and sees three networks.
    struct SeesThree(std::path::PathBuf);
    impl Runner for SeesThree {
        fn run(&mut self, _cmd: &str, args: &[&str]) -> Option<String> {
            if args.contains(&"scan_results") {
                Some(
                    "bssid / frequency / signal level / flags / ssid\n\
                     aa:bb:cc:dd:ee:01\t2412\t-70\t[ESS]\tCafe\n\
                     aa:bb:cc:dd:ee:02\t2412\t-50\t[WPA2-PSK-CCMP][ESS]\tHome\n\
                     aa:bb:cc:dd:ee:03\t2412\t-60\t[SAE][ESS]\tCorp\n"
                        .into(),
                )
            } else {
                Some(String::new())
            }
        }
        fn pause(&mut self, _: Duration) {}
        fn conf_dir(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    #[test]
    fn a_scan_becomes_rows_and_a_missing_adapter_is_explained() {
        use mouli_screen::screens::WifiSecurity;
        let dir = tmp("wifi-scan");
        let sys = dir.join("sys-net");
        std::fs::create_dir_all(sys.join("wlan0/wireless")).unwrap();
        std::fs::create_dir_all(sys.join("eth0")).unwrap();
        let rows = wifi_scan_rows(&mut SeesThree(dir.clone()), &sys).unwrap();
        let got: Vec<(&str, u8, WifiSecurity)> = rows
            .iter()
            .map(|r| (r.ssid.as_str(), r.bars, r.security))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Home", 4, WifiSecurity::Password),
                ("Corp", 3, WifiSecurity::Unsupported),
                ("Cafe", 2, WifiSecurity::Open),
            ]
        );
        std::fs::remove_dir_all(sys.join("wlan0")).unwrap();
        let e = wifi_scan_rows(&mut SeesThree(dir.clone()), &sys).unwrap_err();
        assert!(e.contains("No Wi-Fi adapter"), "{e}");
        assert!(wifi_join_network(&sys, None, "Home", Some("password1")).is_err());
        assert_eq!(wifi_bars(-40), 4);
        assert_eq!(wifi_bars(-90), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn screenshots_are_new_png_files_and_never_replace_one() {
        let dir = tmp("shots");
        let c = Canvas {
            width: 3,
            height: 2,
            pixels: vec![0xFF0000, 0x00FF00, 0x0000FF, 0, 0xFFFFFF, 0x123456],
        };
        let a = save_screenshot(&c, &dir.join("exports")).unwrap();
        let b = save_screenshot(&c, &dir.join("exports")).unwrap();
        assert_eq!(
            (a.as_str(), b.as_str()),
            ("screenshot-1.png", "screenshot-2.png")
        );
        let bytes = std::fs::read(dir.join("exports/screenshot-1.png")).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(bytes, mouli_screen::raster::to_png(&c));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_p_saves_a_screenshot_and_audits_it() {
        let dir = tmp("shot-key");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots.clone()), tx.clone(), rx, device);
        session.data_root = Some(dir.clone());
        tx.send(Ev::Key(Key::Ctrl('p'))).unwrap();
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        run(
            session,
            Scripted {
                replies: vec!["ok"],
                seen: Default::default(),
            },
            "base prompt",
            100_000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        let png = std::fs::read(dir.join("exports/screenshot-1.png")).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let log = std::fs::read_to_string(dir.join("state/audit.log")).unwrap();
        assert!(log.contains("screenshot.save"), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_screen_blanks_after_the_idle_time_and_the_waking_key_does_not_act() {
        let t0 = Instant::now();
        let mut i = IdleBlank::new(5, t0);
        assert_eq!(i.wait(t0), Some(Duration::from_secs(300)));
        assert_eq!(
            i.wait(t0 + Duration::from_secs(100)),
            Some(Duration::from_secs(200))
        );
        assert!(!i.due(t0 + Duration::from_secs(299)));
        assert!(i.due(t0 + Duration::from_secs(300)));
        assert!(i.blanked);
        // It blanks once, and waits for input (no timer) while blank.
        assert!(!i.due(t0 + Duration::from_secs(400)));
        assert_eq!(i.wait(t0 + Duration::from_secs(400)), None);
        // The first input wakes it and is swallowed; the next acts and restarts the timer.
        assert!(i.input(t0 + Duration::from_secs(500)));
        assert!(!i.blanked);
        assert!(!i.input(t0 + Duration::from_secs(501)));
        assert_eq!(
            i.wait(t0 + Duration::from_secs(501)),
            Some(Duration::from_secs(300))
        );
        // Off never blanks.
        let off = IdleBlank::new(0, t0);
        assert_eq!(off.wait(t0 + Duration::from_secs(99999)), None);
    }

    #[test]
    fn the_idle_setting_defaults_validates_and_reloads() {
        let dir = tmp("idle");
        let store = crate::persist::Store::open(&dir).unwrap();
        assert_eq!(load_idle(Some(&store)), 10);
        assert_eq!(load_idle(None), 10);
        store.set(IDLE_KEY, "30").unwrap();
        assert_eq!(load_idle(Some(&store)), 30);
        store.set(IDLE_KEY, "0").unwrap();
        assert_eq!(load_idle(Some(&store)), 0);
        store.set(IDLE_KEY, "7").unwrap();
        assert_eq!(load_idle(Some(&store)), 10, "an unknown value falls back");
        assert_eq!(idle_label(0), "off");
        assert_eq!(idle_label(5), "5 min");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_panic_puts_the_console_back_before_the_message() {
        let before = RESTORES.load(Ordering::SeqCst);
        restore_console_on_panic();
        let r = std::panic::catch_unwind(|| panic!("a deliberate test panic"));
        assert!(r.is_err());
        assert!(
            RESTORES.load(Ordering::SeqCst) > before,
            "the hook restored the console"
        );
        // Calling it directly, repeatedly and without a saved state is harmless.
        restore_console();
        restore_console();
        let _ = std::panic::take_hook();
    }

    #[test]
    fn terminate_ends_the_desktop_cleanly() {
        let dir = tmp("terminate");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let session = Session::new(Shots(shots), tx.clone(), rx, device);
        tx.send(Ev::Terminate).unwrap();
        let end = run(
            session,
            Scripted {
                replies: vec!["ok"],
                seen: Default::default(),
            },
            "base prompt",
            100_000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        assert!(matches!(end, End::Quit));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shaping_the_picture_scales_and_recolours() {
        let c = mouli_screen::raster::Canvas {
            width: 2,
            height: 1,
            pixels: vec![
                mouli_screen::grid::theme::BG.0,
                mouli_screen::grid::theme::TEXT.0,
            ],
        };
        let plain = shape(
            c.clone(),
            ViewOpts {
                contrast: false,
                scale: 1,
            },
        );
        assert_eq!(plain, c);
        let big = shape(
            c,
            ViewOpts {
                contrast: true,
                scale: 2,
            },
        );
        assert_eq!((big.width, big.height), (4, 2));
        assert_eq!(big.pixels[0], 0);
        assert_eq!(big.pixels[3], 0xFFFFFF);
    }

    #[test]
    fn the_time_zone_command_validates_shows_and_saves() {
        let offset = AtomicI32::new(0);
        let dir = tmp("tz");
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::persist::Store::open(&dir).unwrap();
        assert!(time_zone_command("", &offset, Some(&store)).contains("shows UTC"));
        let ok = time_zone_command(" +05:30 ", &offset, Some(&store));
        assert!(ok.contains("UTC+05:30") && ok.contains("kept"), "{ok}");
        assert_eq!(offset.load(Ordering::Relaxed), 330);
        assert_eq!(
            store
                .settings()
                .get(sysinfo::OFFSET_KEY)
                .map(String::as_str),
            Some("330")
        );
        assert!(time_zone_command("", &offset, Some(&store)).contains("UTC+05:30"));
        for bad in ["+99", "x", "+5:99", "1e9", "UTC+25"] {
            let msg = time_zone_command(bad, &offset, Some(&store));
            assert!(msg.contains("not a time zone"), "{bad}: {msg}");
            assert_eq!(offset.load(Ordering::Relaxed), 330, "{bad} changes nothing");
        }
        let no_disk = time_zone_command("-8", &offset, None);
        assert!(no_disk.contains("forgotten at power-off"), "{no_disk}");
        assert_eq!(offset.load(Ordering::Relaxed), -480);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_time_zone_in_the_chat_sets_the_zone_and_keeps_it_on_the_data_disk() {
        let dir = tmp("tzchat");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots), tx.clone(), rx, device);
        session.settings = crate::persist::Store::open(&dir).ok();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for e in keys_for("/time.zone +05:30\n") {
            tx.send(e).unwrap();
        }
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        let end = run(
            session,
            Counting(calls.clone()),
            "sys",
            1000,
            &ModelsInfo::only("t"),
        )
        .unwrap();
        assert_eq!(end, End::Quit);
        let saved = crate::persist::Store::open(&dir).unwrap().settings();
        assert_eq!(
            saved.get(sysinfo::OFFSET_KEY).map(String::as_str),
            Some("330")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "the model was not asked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn click_at(tx: &Sender<Ev>, from: &mut (i32, i32), to: (i32, i32)) {
        use mouli_desk::input::Button;
        tx.send(Ev::Mouse(MouseEvent::Move {
            dx: to.0 - from.0,
            dy: to.1 - from.1,
        }))
        .unwrap();
        tx.send(Ev::Mouse(MouseEvent::Down(Button::Left))).unwrap();
        tx.send(Ev::Mouse(MouseEvent::Up(Button::Left))).unwrap();
        *from = to;
    }

    #[test]
    fn the_power_button_shuts_down_or_restarts_through_the_event_loop() {
        use mouli_desk::desktop::Desktop;
        use mouli_desk::status::{layout, StatusItem};
        for (button, want) in [(0usize, End::Quit), (1, End::Reboot)] {
            let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let (tx, rx) = channel();
            let session = Session::new(Shots(shots), tx.clone(), rx, build_device(None).unwrap());
            // The same desktop the loop builds (the test display is 800 by 600) tells where to click.
            let mut d = Desktop::new(800, 600);
            let power = layout(800, &d.system)
                .into_iter()
                .find(|(i, _)| *i == StatusItem::Power)
                .unwrap()
                .1;
            d.show_power_dialog();
            let (_, rects) = d.alert_layout(&d.alert.clone().unwrap());
            let mut at = (400, 300);
            click_at(&tx, &mut at, (power.x + power.w / 2, 8));
            click_at(&tx, &mut at, (rects[button].x + 10, rects[button].y + 10));
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let end = run(
                session,
                Counting(calls),
                "sys",
                1000,
                &ModelsInfo::only("t"),
            )
            .unwrap();
            assert_eq!(end, want, "button {button}");
        }
    }

    #[test]
    fn a_status_event_reaches_the_menu_bar() {
        use mouli_desk::status::{Bluetooth, NetKind, Network};
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let session = Session::new(
            Shots(shots.clone()),
            tx.clone(),
            rx,
            build_device(None).unwrap(),
        );
        // Quit after the status event has been drawn.
        let s = SystemStatus {
            time: "08:15".into(),
            date: "Thu 26 Nov".into(),
            zone: "UTC".into(),
            network: Network {
                kind: NetKind::Wifi,
                name: "N".into(),
                bars: 4,
            },
            bluetooth: Bluetooth::On,
            battery: None,
        };
        tx.send(Ev::Status(Box::new(s))).unwrap();
        let t2 = tx.clone();
        let seen = shots.clone();
        let quitter = std::thread::spawn(move || {
            // Quit once the picture with the new status has been drawn (start, then the status).
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while seen.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        run(
            session,
            Counting(calls),
            "sys",
            1000,
            &ModelsInfo::only("t"),
        )
        .unwrap();
        quitter.join().unwrap();
        // The first picture (before the status) and the last (after it) differ in the system area.
        let pics = shots.lock().unwrap();
        assert!(
            pics.len() >= 2,
            "drawn at start and after the batch: {}",
            pics.len()
        );
        let (first, last) = (&pics[0], &pics[pics.len() - 1]);
        let left = mouli_desk::status::left_edge(
            800,
            &SystemStatus {
                time: "08:15".into(),
                date: "Thu 26 Nov".into(),
                zone: "UTC".into(),
                network: Network {
                    kind: NetKind::Wifi,
                    name: "N".into(),
                    bars: 4,
                },
                bluetooth: Bluetooth::On,
                battery: None,
            },
        );
        let differs = (left..800).any(|x| {
            (0..mouli_desk::menu::MENU_H)
                .any(|y| first.get(x as usize, y as usize) != last.get(x as usize, y as usize))
        });
        assert!(differs, "the bar changed");
    }

    /// Answers from a script, one reply per call; repeats the last one when it runs out.
    struct Scripted {
        replies: Vec<&'static str>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<Turn>>>>,
    }
    impl Backend for Scripted {
        fn reply(&mut self, history: &[Turn]) -> Result<String, String> {
            let mut seen = self.seen.lock().unwrap();
            let n = seen.len();
            seen.push(history.to_vec());
            Ok(self.replies[n.min(self.replies.len() - 1)].to_string())
        }
    }

    fn run_script(
        replies: Vec<&'static str>,
        typed: &str,
        wait_for_calls: usize,
    ) -> (Vec<Vec<Turn>>, Shots) {
        let dir = tmp("toolloop");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let session = Session::new(Shots(shots.clone()), tx.clone(), rx, device);
        for e in keys_for(typed) {
            tx.send(e).unwrap();
        }
        let (seen2, t2) = (seen.clone(), tx.clone());
        let quitter = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while seen2.lock().unwrap().len() < wait_for_calls && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(300));
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        let backend = Scripted {
            replies,
            seen: seen.clone(),
        };
        run(
            session,
            backend,
            "base prompt",
            100_000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        quitter.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let got = seen.lock().unwrap().clone();
        (got, Shots(shots))
    }

    /// Streams "tick " pieces until told to stop (the first question), then answers briefly.
    struct Endless {
        stopped: std::sync::Arc<AtomicBool>,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Backend for Endless {
        fn reply(&mut self, _: &[Turn]) -> Result<String, String> {
            Ok("done".into())
        }
        fn reply_streaming(
            &mut self,
            _: &[Turn],
            on_piece: &mut dyn FnMut(&str) -> bool,
        ) -> Result<String, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n > 0 {
                on_piece("fine");
                return Ok("fine".into());
            }
            let mut text = String::new();
            for _ in 0..2000 {
                text.push_str("tick ");
                if !on_piece("tick ") {
                    self.stopped.store(true, Ordering::SeqCst);
                    return Ok(text);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(text)
        }
    }

    #[test]
    fn ctrl_c_stops_a_streaming_answer_and_the_next_question_still_works() {
        let dir = tmp("cancel");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let session = Session::new(Shots(shots.clone()), tx.clone(), rx, device);
        for e in keys_for("first\n") {
            tx.send(e).unwrap();
        }
        let stopped = std::sync::Arc::new(AtomicBool::new(false));
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (st, ca, t2) = (stopped.clone(), calls.clone(), tx.clone());
        let driver = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            // Let some pieces arrive, then stop the answer.
            while ca.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(60));
            t2.send(Ev::Key(Key::Ctrl('c'))).unwrap();
            while !st.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(100));
            for e in keys_for("second\n") {
                t2.send(e).unwrap();
            }
            while ca.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(200));
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        run(
            session,
            Endless {
                stopped: stopped.clone(),
                calls: calls.clone(),
            },
            "base prompt",
            100_000,
            &ModelsInfo::only("tiny"),
        )
        .unwrap();
        driver.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            stopped.load(Ordering::SeqCst),
            "the backend was told to stop"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the second question was answered"
        );
    }

    #[test]
    fn the_model_can_call_a_tool_and_gets_the_result_back() {
        let (calls, _) = run_script(
            vec!["TOOL: /code.status", "All good, nothing pending."],
            "what is the state of the code?\n",
            2,
        );
        assert_eq!(
            calls.len(),
            2,
            "one call for the question, one after the tool result"
        );
        let first = &calls[0];
        assert!(
            first[0].content.contains("base prompt") && first[0].content.contains("TOOL: /name"),
            "the tools are described"
        );
        assert!(first[0].content.contains("/code.status"));
        let second = &calls[1];
        let last = second.last().unwrap();
        assert_eq!(last.role, "user");
        assert!(
            last.content.starts_with("Tool result for `/code.status`"),
            "{}",
            last.content
        );
        assert!(last.content.contains("not instructions"));
    }

    #[test]
    fn the_loop_stops_at_the_step_limit() {
        let (calls, _) = run_script(vec!["TOOL: /code.status"], "go\n", toolloop::MAX_STEPS + 1);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            calls.len(),
            toolloop::MAX_STEPS + 1,
            "the question plus {} tool rounds",
            toolloop::MAX_STEPS
        );
    }

    #[test]
    fn an_unknown_tool_is_not_run_and_ends_the_loop() {
        let (calls, _) = run_script(vec!["TOOL: /rm.everything now"], "hi\n", 1);
        assert_eq!(calls.len(), 1, "no second round");
    }

    #[test]
    fn a_plain_answer_makes_no_tool_call() {
        let (calls, _) = run_script(vec!["Just text."], "hi\n", 1);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn the_desktop_records_the_conversation_the_tool_call_and_the_approval_in_the_graph() {
        let dir = tmp("graphe2e");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots), tx.clone(), rx, device);
        session.graph = Recorder::open(&dir, 1000);
        for e in keys_for("please look at the project\n") {
            tx.send(e).unwrap();
        }
        let (seen2, t2) = (seen.clone(), tx.clone());
        let quitter = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while seen2.lock().unwrap().len() < 2 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(300));
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        let backend = Scripted {
            replies: vec!["TOOL: /code.files", "There are no files."],
            seen: seen.clone(),
        };
        run(session, backend, "base", 100_000, &ModelsInfo::only("tiny")).unwrap();
        quitter.join().unwrap();
        let back = Recorder::open(&dir, 2000);
        assert!(back.warning.is_none(), "{:?}", back.warning);
        let g = back.graph();
        let texts: Vec<String> = g
            .nodes_of("message")
            .iter()
            .map(|n| format!("{}:{}", n.props["role"], n.props["text"]))
            .collect();
        assert!(
            texts.iter().any(|t| t == "user:please look at the project"),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "assistant:TOOL: /code.files"),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "assistant:There are no files."),
            "{texts:?}"
        );
        let call = &g.nodes_of("toolcall")[0];
        assert_eq!(call.props["tool"], "code.files");
        let by = g.out_edges(call.id, Some("run_by"))[0].to;
        assert_eq!(g.get(by).unwrap().key, "agent:chat");
        assert!(g.find("model", "tiny").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_graph_commands_show_and_erase_the_record() {
        let dir = tmp("graphcmd");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots), tx.clone(), rx, device);
        session.graph = Recorder::open(&dir, 1);
        for line in [
            "/code.files\n",
            "/graph\n",
            "/graph.file nothing.rs\n",
            "/graph.forget\n",
            "/graph.what\n",
        ] {
            for e in keys_for(line) {
                tx.send(e).unwrap();
            }
        }
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        run(
            session,
            Counting(calls.clone()),
            "s",
            1000,
            &ModelsInfo::only("t"),
        )
        .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "none of these reach the model"
        );
        assert!(
            !dir.join(crate::datagraph::LOG_NAME).exists(),
            "forgotten at the end"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_agent_change_is_recorded_as_a_held_approval_linked_to_its_file() {
        let dir = tmp("graphapproval");
        std::fs::create_dir_all(dir.join("projects")).unwrap();
        std::fs::write(dir.join("projects/a.txt"), "old\n").unwrap();
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots), tx.clone(), rx, device);
        session.graph = Recorder::open(&dir, 1000);
        for e in keys_for("change a.txt\n") {
            tx.send(e).unwrap();
        }
        let (seen2, t2) = (seen.clone(), tx.clone());
        let quitter = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while seen2.lock().unwrap().len() < 3 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(300));
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        let backend = Scripted {
            replies: vec![
                "TOOL: /code.edit path=a.txt text=new",
                "TOOL: /code.apply",
                "It is waiting for your approval.",
            ],
            seen: seen.clone(),
        };
        run(session, backend, "base", 100_000, &ModelsInfo::only("tiny")).unwrap();
        quitter.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("projects/a.txt")).unwrap(),
            "old\n",
            "nothing written without approval"
        );
        let back = Recorder::open(&dir, 2000);
        let g = back.graph();
        let apr = &g.nodes_of("approval")[0];
        assert_eq!(apr.props["capability"], "code.apply");
        assert_eq!(apr.props["status"], "waiting");
        let call = g.in_edges(apr.id, Some("held_for"))[0].from;
        assert_eq!(g.get(call).unwrap().props["tool"], "code.apply");
        let file = g.find("file", "a.txt").unwrap();
        assert_eq!(g.in_edges(file.id, Some("touched"))[0].props["how"], "edit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn storage_run(loaded: &str, keys: Vec<Ev>) -> std::path::PathBuf {
        let dir = tmp("storage");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models/a.gguf"), vec![1u8; 100]).unwrap();
        std::fs::write(dir.join("models/b.gguf"), vec![1u8; 50]).unwrap();
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let mut session = Session::new(Shots(shots), tx.clone(), rx, device);
        session.data_root = Some(dir.clone());
        session.graph = Recorder::open(&dir, 1);
        for k in keys {
            tx.send(k).unwrap();
        }
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        run(
            session,
            Counting(calls),
            "s",
            1000,
            &ModelsInfo::only(loaded),
        )
        .unwrap();
        dir
    }

    #[test]
    fn the_storage_window_removes_a_model_after_two_presses_and_records_it() {
        let dir = storage_run(
            "other",
            vec![
                Ev::Key(Key::F(11)),
                Ev::Key(Key::Char('d')),
                Ev::Key(Key::Char('d')),
            ],
        );
        assert!(
            !dir.join("models/a.gguf").exists(),
            "the largest model, first in the list, was removed"
        );
        assert!(dir.join("models/b.gguf").exists());
        let back = Recorder::open(&dir, 9);
        assert_eq!(
            back.graph().find("model", "a.gguf").unwrap().props["status"],
            "removed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_press_or_the_wrong_keys_remove_nothing() {
        let dir = storage_run(
            "other",
            vec![
                Ev::Key(Key::F(11)),
                Ev::Key(Key::Char('d')),
                Ev::Key(Key::Down),
                Ev::Key(Key::Char('d')),
                Ev::Key(Key::Char('x')),
                Ev::Key(Key::Esc),
                Ev::Key(Key::Char('r')),
            ],
        );
        assert!(dir.join("models/a.gguf").exists() && dir.join("models/b.gguf").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_model_in_use_cannot_be_removed() {
        let dir = storage_run(
            "a",
            vec![
                Ev::Key(Key::F(11)),
                Ev::Key(Key::Char('d')),
                Ev::Key(Key::Char('d')),
            ],
        );
        assert!(dir.join("models/a.gguf").exists(), "a is the loaded model");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
