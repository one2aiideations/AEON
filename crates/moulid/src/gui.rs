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

use crate::ui::{trim_history, Backend, End, Turn};
use mouli_desk::desktop::Desktop;
use mouli_desk::input::{EvdevDecoder, MouseEvent, TouchpadDecoder};
use mouli_device::{Backend as DeviceBackend, Device, Outcome};
use mouli_interface::routing::{ModelEntry, Route};
use mouli_interface::{Permissions, Policy, Workspace};
use mouli_screen::key::Key;
use mouli_screen::raster::Canvas;
use mouli_screen::screen::{Action, Tab};
use mouli_screen::screens::Role;
use mouli_screen::shell::Shell;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

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
    /// The mouse did something.
    Mouse(MouseEvent),
    /// A key was pressed.
    Key(Key),
    /// The model finished answering (or failed).
    Reply(Result<String, String>),
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
    let mut summary = InputSummary::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return summary;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let node = entry.file_name().to_string_lossy().into_owned();
        if !node.starts_with("event") {
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

// ------------------------------------------------------------------ console

/// Keeps typed characters from being echoed onto the framebuffer console while the desktop is
/// up, and puts the terminal back afterwards.
#[cfg(target_os = "linux")]
struct TtyGuard {
    saved: Option<nix::sys::termios::Termios>,
}

#[cfg(target_os = "linux")]
impl TtyGuard {
    fn new() -> Self {
        use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
        let stdin = std::io::stdin();
        let saved = tcgetattr(&stdin).ok();
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
        use nix::sys::termios::{tcflush, tcsetattr, FlushArg, SetArg};
        let stdin = std::io::stdin();
        let _ = tcflush(&stdin, FlushArg::TCIFLUSH);
        if let Some(t) = &self.saved {
            let _ = tcsetattr(&stdin, SetArg::TCSANOW, t);
        }
        let _ = write!(std::io::stdout(), "\x1b[?25h\x1b[2J\x1b[H");
        let _ = std::io::stdout().flush();
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

/// What the Player window uses: there is no video player in this image, so it says so instead of
/// pretending to play.
struct NoVideo;

impl mouli_player::Backend for NoVideo {
    fn load(&mut self, _: &mouli_player::Source) -> Result<(), String> {
        Err("There is no video player in this image yet.".into())
    }
    fn pause(&mut self, _: bool) {}
    fn seek(&mut self, _: u32) {}
    fn set_volume(&mut self, _: u8) {}
    fn set_speed(&mut self, _: u16) {}
    fn stop(&mut self) {}
}

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

/// Builds the governed workspace with the Sheet, Studio, Code and Player applications, and the
/// device that connects the screens to it.
///
/// With `data_dir` (the MOULI-DATA partition) the sheet and document are saved under
/// `<data_dir>/apps` after every change and the Code window opens `<data_dir>/projects`.
/// Without it nothing is saved.
///
/// # Errors
/// A message when the workspace cannot be set up.
pub fn build_device(data_dir: Option<&Path>) -> Result<Device<NoRoute>, String> {
    let fail = |e: &dyn std::fmt::Display| format!("cannot set up the applications: {e}");
    let mut ws = Workspace::new(Policy::default(), Default::default()).map_err(|e| fail(&e))?;
    ws.add_role(Permissions::new("owner", &["*"]));
    ws.add_model(ModelEntry {
        id: "local".into(),
        provider: "local".into(),
        local: true,
        caps: BTreeSet::from(["text".to_string()]),
        context: 4096,
        cost_per_1k: 0,
        quality: 50,
    });
    let apps = mouli_apps::register(&mut ws).map_err(|e| fail(&e))?;
    let code = mouli_apps::register_code(&mut ws).map_err(|e| fail(&e))?;
    let player = mouli_apps::register_player(&mut ws, Box::new(NoVideo)).map_err(|e| fail(&e))?;
    let mut device = Device::new(ws, NoRoute, TENANT, USER, "owner").map_err(|e| fail(&e))?;
    device.attach_apps(apps);
    device.attach_code(code);
    device.attach_player(player);
    if let Some(dir) = data_dir {
        let apps_dir = dir.join("apps");
        std::fs::create_dir_all(&apps_dir).map_err(|e| fail(&e))?;
        device.keep_apps_in(&apps_dir)?;
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
    let width = i32::try_from(display.size().0).unwrap_or(1024);
    let inputs = spawn_inputs(Path::new(INPUT_DIR), Path::new(INPUT_SYS), width, &tx);
    if inputs.keyboards.is_empty() && !inputs.has_pointer() {
        return Err("no keyboard, mouse or touchpad found in /dev/input".into());
    }
    let mut s = Session::new(display, tx, rx, device);
    s.saving = data_dir.is_some();
    s.inputs = inputs;
    Ok(s)
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
/// thinks. Sheet, Studio, Code, Player, Approvals, Audit and Security run through the governed
/// workspace of the device.
///
/// # Errors
/// An error from drawing to the display.
pub fn run<B, D>(
    session: Session<D>,
    backend: B,
    system_prompt: &str,
    max_tokens: usize,
    model_name: &str,
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
    } = session;
    let _tty = TtyGuard::new();
    let (w, h) = display.size();
    let mut desktop = Desktop::new(
        i32::try_from(w).unwrap_or(1024),
        i32::try_from(h).unwrap_or(768),
    );
    desktop.status = if saving {
        "MOULI OS".into()
    } else {
        "MOULI OS - nothing is saved (no MOULI-DATA disk)".into()
    };
    device.shell.chat.model = model_name.into();
    desktop.open(&mut device.shell, Tab::Chat);
    if !inputs.has_pointer() {
        device.shell.chat.push(
            Role::Notice,
            "No mouse or touchpad found. Plug in a USB mouse, or use the keyboard: F1 to F10 open windows, Ctrl+Q shuts down.",
        );
    }
    device.shell.chat.push(Role::Notice, &inputs.line());
    let mut history = fresh(system_prompt);

    let (req_tx, req_rx) = channel::<Vec<Turn>>();
    let reply_tx = tx.clone();
    std::thread::spawn(move || {
        let mut backend = backend;
        while let Ok(turns) = req_rx.recv() {
            if reply_tx.send(Ev::Reply(backend.reply(&turns))).is_err() {
                break;
            }
        }
    });

    let started = Instant::now();
    let now_ms = move || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    display.present(&desktop.render(&device.shell))?;
    loop {
        let Ok(first) = rx.recv() else {
            return Ok(End::Eof);
        };
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
            if batch.len() >= 256 {
                break;
            }
        }
        for ev in batch {
            let action = match ev {
                Ev::Mouse(m) => desktop.on_mouse(&mut device.shell, m, now_ms()),
                Ev::Key(k) => desktop.on_key(&mut device.shell, k),
                Ev::Reply(r) => {
                    on_reply(&mut device.shell, &mut history, r);
                    Action::None
                }
            };
            match action {
                Action::None => {}
                Action::Show(tab) => desktop.open(&mut device.shell, tab),
                Action::Send(text) => {
                    if !device.shell.chat.busy {
                        device.shell.chat.push(Role::User, &text);
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
                        history = fresh(system_prompt);
                    }
                }
                other => match device.run(other) {
                    Outcome::Quit => return Ok(End::Quit),
                    Outcome::Handled => {}
                    Outcome::Host(_) => device.shell.chat.push(
                        Role::Notice,
                        "That is not available in the desktop yet; restart with mouli.text=1 for the text menu.",
                    ),
                },
            }
        }
        display.present(&desktop.render(&device.shell))?;
    }
}

fn on_reply(shell: &mut Shell, history: &mut Vec<Turn>, reply: Result<String, String>) {
    shell.chat.busy = false;
    match reply {
        Ok(text) => {
            shell.chat.push(Role::Assistant, &text);
            history.push(Turn {
                role: "assistant",
                content: text,
            });
        }
        Err(e) => {
            if history.last().is_some_and(|t| t.role == "user") {
                history.pop();
            }
            shell
                .chat
                .push(Role::Notice, &format!("The model could not answer: {e}"));
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
        let quitter = std::thread::spawn(move || {
            // Wait until the reply has been drawn, then shut down.
            std::thread::sleep(std::time::Duration::from_millis(600));
            t2.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        });
        let end = run(session, Echo, "be nice", 10_000, "tiny").unwrap();
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
        assert!(d.join("apps").is_dir() && d.join("projects").is_dir());
        // A data disk that cannot be written is an error, not a panic.
        assert!(build_device(Some(Path::new("/proc/mouli-nonexistent"))).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sheet_cell_typed_in_the_desktop_is_saved_on_the_data_disk() {
        let dir = tmp("sheet");
        let shots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let device = build_device(Some(&dir)).unwrap();
        let session = Session::new(Shots(shots), tx.clone(), rx, device);
        tx.send(Ev::Key(Key::F(7))).unwrap();
        for e in keys_for("424242\n") {
            tx.send(e).unwrap();
        }
        tx.send(Ev::Key(Key::Ctrl('q'))).unwrap();
        let end = run(session, Echo, "sys", 1000, "tiny").unwrap();
        assert_eq!(end, End::Quit);
        let saved = std::fs::read_to_string(dir.join("apps/sheet.mouli")).unwrap();
        assert!(saved.contains("424242"), "saved sheet: {saved}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_player_says_plainly_that_it_cannot_play() {
        use mouli_player::Backend as _;
        let mut b = NoVideo;
        let src = mouli_player::source::parse("https://youtu.be/dQw4w9WgXcQ").unwrap();
        assert!(b.load(&src).unwrap_err().contains("no video player"));
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
}
