//! The system area at the right end of the menu bar: Bluetooth, network, battery, the clock and the
//! power button.
//!
//! The host (`moulid`) reads the machine and fills in a [`SystemStatus`]; this module lays the items
//! out, draws their icons in software, and says what to show when one is clicked. Nothing here
//! reads the system, so everything is tested without hardware.

use crate::geom::Rect;
use crate::menu::MENU_H;
use crate::paint::{fill, line, outline, px, text, text_width, vline};
use crate::theme::{DISABLED, HOVER, ON_SURFACE, ON_SURFACE_DIM, PRIMARY};
use mouli_screen::raster::Canvas;
use mouli_screen::typeface::{ui_medium, ui_small};

/// Colour for a low battery (Material red 400).
pub const LOW: u32 = 0xEF5350;
/// The widest an icon cell is.
const ICON_W: i32 = 30;
/// Space left at the right edge.
const EDGE: i32 = 6;

/// What kind of network the machine is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetKind {
    /// Not connected.
    #[default]
    None,
    /// A cable.
    Ethernet,
    /// Wi-Fi.
    Wifi,
}

/// The network connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Network {
    /// How it is connected.
    pub kind: NetKind,
    /// The network or adapter name (the Wi-Fi name, or `eth0`).
    pub name: String,
    /// Signal strength from 0 to 4 (Wi-Fi only).
    pub bars: u8,
}

/// The Bluetooth state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bluetooth {
    /// No Bluetooth adapter: nothing is shown.
    #[default]
    NoAdapter,
    /// An adapter, not switched on.
    Off,
    /// Switched on, nothing connected.
    On,
    /// Switched on with this many devices connected.
    Connected(u8),
}

/// The battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Charge, 0 to 100.
    pub percent: u8,
    /// Whether it is charging.
    pub charging: bool,
}

/// Everything the system area shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemStatus {
    /// The time, for example `17:42`.
    pub time: String,
    /// The date, for example `Tue 25 Nov`.
    pub date: String,
    /// The time zone, for example `UTC+05:30`.
    pub zone: String,
    /// The network.
    pub network: Network,
    /// Bluetooth.
    pub bluetooth: Bluetooth,
    /// The battery, on machines that have one.
    pub battery: Option<Battery>,
}

/// An item of the system area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItem {
    /// Bluetooth.
    Bluetooth,
    /// Network.
    Network,
    /// Battery.
    Battery,
    /// Date and time.
    Clock,
    /// The power button.
    Power,
}

impl SystemStatus {
    fn clock_text(&self) -> String {
        if self.date.is_empty() {
            self.time.clone()
        } else {
            format!("{}  {}", self.date, self.time)
        }
    }

    fn battery_text(b: &Battery) -> String {
        format!("{}%", b.percent)
    }
}

/// Where each item is in a bar `width` pixels wide, left to right. Items that do not apply (no
/// Bluetooth adapter, no battery) are left out.
#[must_use]
pub fn layout(width: i32, s: &SystemStatus) -> Vec<(StatusItem, Rect)> {
    let mut widths: Vec<(StatusItem, i32)> = Vec::new();
    if s.bluetooth != Bluetooth::NoAdapter {
        widths.push((StatusItem::Bluetooth, ICON_W));
    }
    widths.push((StatusItem::Network, ICON_W));
    if let Some(b) = &s.battery {
        let t = SystemStatus::battery_text(b);
        widths.push((StatusItem::Battery, 28 + text_width(ui_small(), &t) + 10));
    }
    widths.push((
        StatusItem::Clock,
        text_width(ui_medium(), &s.clock_text()) + 20,
    ));
    widths.push((StatusItem::Power, ICON_W));
    let mut x = width - EDGE;
    let mut out = Vec::with_capacity(widths.len());
    for (item, w) in widths.into_iter().rev() {
        x -= w;
        out.push((item, Rect::new(x, 0, w, MENU_H - 1)));
    }
    out.reverse();
    out
}

/// The x position where the system area starts (anything else in the bar must end before it).
#[must_use]
pub fn left_edge(width: i32, s: &SystemStatus) -> i32 {
    layout(width, s).first().map_or(width, |(_, r)| r.x)
}

/// The item at a point in the bar.
#[must_use]
pub fn item_at(width: i32, s: &SystemStatus, x: i32, y: i32) -> Option<StatusItem> {
    layout(width, s)
        .into_iter()
        .find(|(_, r)| r.contains(x, y))
        .map(|(i, _)| i)
}

/// The text of the dialog shown when an item is clicked (not used for the power button).
#[must_use]
pub fn info_lines(item: StatusItem, s: &SystemStatus) -> Vec<String> {
    let mut v = Vec::new();
    match item {
        StatusItem::Network => {
            v.push("Network".into());
            v.push(String::new());
            match s.network.kind {
                NetKind::None => {
                    v.push("Not connected.".into());
                    v.push("Plug in a cable, or restart and press w".into());
                    v.push("in the model menu to join Wi-Fi.".into());
                }
                NetKind::Ethernet => {
                    v.push(format!("Connected by cable ({}).", s.network.name));
                }
                NetKind::Wifi => {
                    v.push(format!("Wi-Fi: {}", s.network.name));
                    v.push(format!("Signal: {} of 4 bars", s.network.bars.min(4)));
                }
            }
        }
        StatusItem::Bluetooth => {
            v.push("Bluetooth".into());
            v.push(String::new());
            match s.bluetooth {
                Bluetooth::NoAdapter => v.push("No Bluetooth adapter found.".into()),
                Bluetooth::Off => {
                    v.push("An adapter is present but not started.".into());
                    v.push("Restart and press b in the model menu to pair.".into());
                }
                Bluetooth::On => v.push("On. No device connected.".into()),
                Bluetooth::Connected(n) => v.push(format!(
                    "On. {n} device{} connected.",
                    if n == 1 { "" } else { "s" }
                )),
            }
        }
        StatusItem::Battery => {
            v.push("Battery".into());
            v.push(String::new());
            match &s.battery {
                Some(b) => v.push(format!(
                    "{}%, {}.",
                    b.percent,
                    if b.charging {
                        "charging"
                    } else {
                        "not charging"
                    }
                )),
                None => v.push("No battery.".into()),
            }
        }
        StatusItem::Clock => {
            v.push("Date and time".into());
            v.push(String::new());
            v.push(format!("{}  {}  ({})", s.date, s.time, s.zone));
            v.push(String::new());
            v.push("To change the time zone, type in the chat:".into());
            v.push("/time.zone +05:30".into());
        }
        StatusItem::Power => {}
    }
    v
}

fn signal_arcs(bars: u8) -> i32 {
    match bars {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 3,
    }
}

fn draw_wifi(c: &mut Canvas, cx: i32, cy: i32, lit: i32, connected: bool) {
    // Three arcs over a dot; the origin is the dot.
    let (ox, oy) = (cx, cy + 6);
    for dy in -14i32..=1 {
        for dx in -12i32..=12 {
            let d = f64::from(dx * dx + dy * dy).sqrt();
            let up = f64::from(-dy);
            // Within about 50 degrees of straight up.
            if up <= 0.0 || f64::from(dx.abs()) > up * 1.2 {
                continue;
            }
            for (i, r) in [4.0f64, 8.0, 12.0].iter().enumerate() {
                if (d - r).abs() < 0.8 {
                    let on = connected && (i as i32) < lit;
                    px(c, ox + dx, oy + dy, if on { ON_SURFACE } else { DISABLED });
                }
            }
        }
    }
    fill(
        c,
        Rect::new(ox - 1, oy - 1, 3, 3),
        if connected { ON_SURFACE } else { DISABLED },
    );
}

fn draw_ethernet(c: &mut Canvas, cx: i32, cy: i32) {
    outline(c, Rect::new(cx - 8, cy - 6, 16, 10), ON_SURFACE);
    for k in 0..4 {
        vline(c, cx - 5 + k * 3, cy - 3, 4, ON_SURFACE);
    }
    vline(c, cx, cy + 4, 4, ON_SURFACE);
}

fn draw_bluetooth(c: &mut Canvas, cx: i32, cy: i32, colour: u32) {
    let p = |x: i32, y: i32| (cx + x, cy + y);
    line(c, p(0, -8), p(0, 8), colour);
    line(c, p(0, -8), p(5, -4), colour);
    line(c, p(5, -4), p(-5, 4), colour);
    line(c, p(-5, 4), p(0, 8), colour);
    line(c, p(0, 8), p(5, 4), colour);
    line(c, p(5, 4), p(-5, -4), colour);
}

fn draw_power(c: &mut Canvas, cx: i32, cy: i32) {
    for dy in -9i32..=9 {
        for dx in -9i32..=9 {
            let d = f64::from(dx * dx + dy * dy).sqrt();
            // A ring with a gap at the top.
            if (d - 7.0).abs() < 0.9 && !(dy < 0 && dx.abs() <= 3) {
                px(c, cx + dx, cy + dy, ON_SURFACE);
            }
        }
    }
    vline(c, cx, cy - 8, 8, ON_SURFACE);
    vline(c, cx + 1, cy - 8, 8, ON_SURFACE);
}

fn draw_battery(c: &mut Canvas, x: i32, cy: i32, b: &Battery) {
    let colour = if b.charging {
        PRIMARY
    } else if b.percent < 15 {
        LOW
    } else {
        ON_SURFACE
    };
    let body = Rect::new(x, cy - 5, 22, 11);
    outline(c, body, ON_SURFACE_DIM);
    fill(c, Rect::new(x + 22, cy - 2, 2, 5), ON_SURFACE_DIM);
    let inner = i32::from(b.percent.min(100)) * 18 / 100;
    if inner > 0 {
        fill(c, Rect::new(x + 2, cy - 3, inner, 7), colour);
    }
}

/// Draws the system area into the bar. `pointer` highlights the item under it.
pub fn draw(c: &mut Canvas, s: &SystemStatus, pointer: (i32, i32)) {
    let width = c.width as i32;
    let cy = (MENU_H - 1) / 2;
    for (item, r) in layout(width, s) {
        if r.contains(pointer.0, pointer.1) {
            fill(c, r, HOVER);
        }
        let cx = r.x + r.w / 2;
        match item {
            StatusItem::Bluetooth => {
                let colour = match s.bluetooth {
                    Bluetooth::Connected(_) => PRIMARY,
                    Bluetooth::On => ON_SURFACE,
                    _ => DISABLED,
                };
                draw_bluetooth(c, cx, cy, colour);
            }
            StatusItem::Network => match s.network.kind {
                NetKind::Ethernet => draw_ethernet(c, cx, cy),
                NetKind::Wifi => draw_wifi(c, cx, cy, signal_arcs(s.network.bars), true),
                NetKind::None => {
                    draw_wifi(c, cx, cy, 0, false);
                    line(c, (cx - 9, cy + 8), (cx + 9, cy - 8), DISABLED);
                }
            },
            StatusItem::Battery => {
                if let Some(b) = &s.battery {
                    draw_battery(c, r.x + 4, cy, b);
                    let face = ui_small();
                    text(
                        c,
                        face,
                        r.x + 4 + 28,
                        cy - face.line_height / 2,
                        &SystemStatus::battery_text(b),
                        ON_SURFACE,
                    );
                }
            }
            StatusItem::Clock => {
                let face = ui_medium();
                text(
                    c,
                    face,
                    r.x + 10,
                    cy - face.line_height / 2,
                    &s.clock_text(),
                    ON_SURFACE,
                );
            }
            StatusItem::Power => draw_power(c, cx, cy),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::canvas;

    fn status() -> SystemStatus {
        SystemStatus {
            time: "17:42".into(),
            date: "Tue 25 Nov".into(),
            zone: "UTC".into(),
            network: Network {
                kind: NetKind::Wifi,
                name: "HomeNet".into(),
                bars: 3,
            },
            bluetooth: Bluetooth::Connected(1),
            battery: Some(Battery {
                percent: 87,
                charging: false,
            }),
        }
    }

    #[test]
    fn items_are_ordered_inside_the_bar_and_never_overlap() {
        for width in [640, 800, 1280, 1920] {
            let l = layout(width, &status());
            let items: Vec<StatusItem> = l.iter().map(|(i, _)| *i).collect();
            assert_eq!(
                items,
                vec![
                    StatusItem::Bluetooth,
                    StatusItem::Network,
                    StatusItem::Battery,
                    StatusItem::Clock,
                    StatusItem::Power
                ]
            );
            for w in l.windows(2) {
                assert!(w[0].1.right() <= w[1].1.x, "{width}: {w:?}");
            }
            let last = l.last().unwrap().1;
            assert!(last.right() <= width, "{width}");
            assert!(l[0].1.x > 300, "{width}: leaves room for the menus");
        }
    }

    #[test]
    fn items_that_do_not_apply_are_left_out() {
        let mut s = status();
        s.bluetooth = Bluetooth::NoAdapter;
        s.battery = None;
        let items: Vec<StatusItem> = layout(1280, &s).iter().map(|(i, _)| *i).collect();
        assert_eq!(
            items,
            vec![StatusItem::Network, StatusItem::Clock, StatusItem::Power]
        );
    }

    #[test]
    fn a_click_finds_the_item_under_it() {
        let s = status();
        let l = layout(1280, &s);
        for (item, r) in &l {
            assert_eq!(item_at(1280, &s, r.x + r.w / 2, 5), Some(*item));
        }
        assert_eq!(item_at(1280, &s, 100, 5), None);
        assert_eq!(
            item_at(1280, &s, l[0].1.x + 2, MENU_H + 5),
            None,
            "below the bar"
        );
        assert_eq!(left_edge(1280, &s), l[0].1.x);
    }

    #[test]
    fn every_state_draws_without_panicking_even_on_tiny_canvases() {
        let nets = [
            Network::default(),
            Network {
                kind: NetKind::Ethernet,
                name: "eth0".into(),
                bars: 0,
            },
            Network {
                kind: NetKind::Wifi,
                name: "x".into(),
                bars: 0,
            },
            Network {
                kind: NetKind::Wifi,
                name: "x".into(),
                bars: 9,
            },
        ];
        let bts = [
            Bluetooth::NoAdapter,
            Bluetooth::Off,
            Bluetooth::On,
            Bluetooth::Connected(3),
        ];
        for n in &nets {
            for b in bts {
                for batt in [
                    None,
                    Some(Battery {
                        percent: 0,
                        charging: false,
                    }),
                    Some(Battery {
                        percent: 100,
                        charging: true,
                    }),
                ] {
                    let s = SystemStatus {
                        network: n.clone(),
                        bluetooth: b,
                        battery: batt,
                        ..status()
                    };
                    for (w, h) in [(0usize, 0usize), (30, 30), (640, 40), (1280, 40)] {
                        let mut c = canvas(w, h, 0);
                        draw(&mut c, &s, (5, 5));
                    }
                }
            }
        }
    }

    #[test]
    fn drawing_changes_pixels_where_the_icons_are() {
        let s = status();
        let mut c = canvas(1280, 40, 0x111111);
        draw(&mut c, &s, (0, 0));
        for (item, r) in layout(1280, &s) {
            let mut lit = 0;
            for y in 0..MENU_H {
                for x in r.x..r.right() {
                    if c.get(x as usize, y as usize) != Some(0x111111) {
                        lit += 1;
                    }
                }
            }
            assert!(lit > 8, "{item:?} drew {lit} pixels");
        }
    }

    #[test]
    fn the_hovered_item_is_highlighted() {
        let s = status();
        let r = layout(1280, &s)[1].1;
        let mut with = canvas(1280, 40, 0);
        draw(&mut with, &s, (r.x + 3, 3));
        let mut without = canvas(1280, 40, 0);
        draw(&mut without, &s, (0, 100));
        assert_ne!(
            with.get((r.x + 3) as usize, 3),
            without.get((r.x + 3) as usize, 3)
        );
    }

    #[test]
    fn info_dialogs_say_what_the_icons_mean() {
        let s = status();
        assert!(info_lines(StatusItem::Network, &s)
            .join(" ")
            .contains("HomeNet"));
        assert!(info_lines(StatusItem::Bluetooth, &s)
            .join(" ")
            .contains("1 device connected"));
        let mut two = s.clone();
        two.bluetooth = Bluetooth::Connected(2);
        assert!(info_lines(StatusItem::Bluetooth, &two)
            .join(" ")
            .contains("2 devices"));
        assert!(info_lines(StatusItem::Battery, &s)
            .join(" ")
            .contains("87%"));
        let clock = info_lines(StatusItem::Clock, &s).join(" ");
        assert!(
            clock.contains("17:42") && clock.contains("/time.zone"),
            "{clock}"
        );
        let mut off = s.clone();
        off.network = Network::default();
        off.bluetooth = Bluetooth::Off;
        off.battery = None;
        assert!(info_lines(StatusItem::Network, &off)
            .join(" ")
            .contains("Not connected"));
        assert!(info_lines(StatusItem::Bluetooth, &off)
            .join(" ")
            .contains("not started"));
        assert!(info_lines(StatusItem::Battery, &off)
            .join(" ")
            .contains("No battery"));
        assert!(info_lines(StatusItem::Power, &s).is_empty());
    }

    #[test]
    fn a_wired_connection_is_described_by_its_adapter() {
        let mut s = status();
        s.network = Network {
            kind: NetKind::Ethernet,
            name: "eth0".into(),
            bars: 0,
        };
        assert!(info_lines(StatusItem::Network, &s)
            .join(" ")
            .contains("by cable (eth0)"));
    }
}
