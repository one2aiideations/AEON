//! Drawing the desktop.

use crate::art::*;
use crate::desktop::{Alert, Desktop};
use crate::geom::Rect;
use crate::paint::*;
use crate::theme::*;
use crate::wm::{Window, STATUS_H};
use mouli_screen::grid::Grid;
use mouli_screen::raster::{render, Canvas, RasterCfg};
use mouli_screen::shell::Shell;

fn button(c: &mut Canvas, r: Rect, label: &str, default: bool) {
    if default {
        outline(c, r, BLACK);
    }
    let b = r.inset(if default { 2 } else { 0 });
    fill(c, b, FACE);
    outline(c, b, BLACK);
    bevel(c, b.inset(1), LIGHT, DARK);
    let w = text_width(label, 2);
    text(
        c,
        b.x + (b.w - w) / 2,
        b.y + (b.h - 16) / 2,
        label,
        BLACK,
        2,
    );
}

fn draw_window(c: &mut Canvas, w: &Window, active: bool, shell: &Shell) {
    let o = w.outer();
    fill(c, Rect::new(o.x + 3, o.y + 3, o.w, o.h), SHADOW);
    fill(c, o, FACE);
    outline(c, o, BLACK);

    let t = w.title_bar();
    bevel(c, t, LIGHT, SHADE);
    if active {
        for y in (t.y + 2)..(t.bottom() - 2) {
            let colour = if (y - t.y) % 2 == 0 {
                STRIPE_A
            } else {
                STRIPE_B
            };
            hline(c, t.x + 2, y, t.w - 4, colour);
        }
    }
    let title = w.tab.label();
    let tw = text_width(title, 2);
    let tx = t.x + (t.w - tw) / 2;
    fill(c, Rect::new(tx - 8, t.y + 2, tw + 16, t.h - 4), FACE);
    text(
        c,
        tx,
        t.y + (t.h - 16) / 2,
        title,
        if active { BLACK } else { DISABLED },
        2,
    );
    hline(c, t.x, t.bottom(), t.w, BLACK);
    if active {
        for b in [w.close_box(), w.zoom_box()] {
            fill(c, b, FACE);
            outline(c, b, BLACK);
            bevel(c, b.inset(1), LIGHT, SHADE);
        }
        let z = w.zoom_box();
        outline(c, Rect::new(z.x + 3, z.y + 3, 8, 8), BLACK);
        outline(c, Rect::new(z.x + 3, z.y + 3, 5, 5), BLACK);
    }

    // The screen inside the window.
    let k = w.content();
    let mut grid = Grid::new(w.cols as usize, w.rows as usize);
    let area = grid.area();
    shell.screen(w.tab).draw(&mut grid, area);
    let picture = render(&grid, RasterCfg::default());
    blit(c, &picture, k.x, k.y, k);

    // The strip under it: key hints, and the grow box.
    let s = w.status_strip();
    fill(c, s, FACE);
    hline(c, s.x, s.y, s.w, DARK);
    let hints = shell.screen(w.tab).hints();
    let room = ((s.w - 30) / 8).max(0) as usize;
    let shown: String = hints.chars().take(room).collect();
    text(
        c,
        s.x + 8,
        s.y + (STATUS_H - 8) / 2 + 1,
        &shown,
        0x333333,
        1,
    );
    let g = w.grow_box();
    for i in 0..3 {
        let off = 4 + i * 4;
        for n in 0..(off - 2).max(1) {
            px(
                c,
                g.right() - 3 - n,
                g.bottom() - 3 - (off - 1 - n).max(0),
                DARK,
            );
        }
    }
}

fn draw_alert(c: &mut Canvas, d: &Desktop, alert: &Alert) {
    let (r, ok) = d.alert_rects(alert);
    fill(c, Rect::new(r.x + 4, r.y + 4, r.w, r.h), SHADOW);
    fill(c, r, FACE);
    outline(c, r, BLACK);
    bevel(c, r.inset(1), LIGHT, DARK);
    bevel(c, r.inset(3), DARK, LIGHT);
    for (i, line) in alert.lines.iter().enumerate() {
        text(c, r.x + 32, r.y + 24 + i as i32 * 22, line, BLACK, 2);
    }
    button(c, ok, "OK", true);
}

fn draw_icons(c: &mut Canvas, d: &Desktop) {
    for (i, spot) in d.icons.iter().enumerate() {
        let r = d.icon_rect(i);
        let art: &Icon = match spot.tab {
            None => &DISK,
            Some(mouli_screen::screen::Tab::Chat) => &CHAT,
            Some(mouli_screen::screen::Tab::Models) => &MODELS,
            Some(mouli_screen::screen::Tab::Approvals) => &APPROVALS,
            Some(mouli_screen::screen::Tab::Audit) => &AUDIT,
            Some(mouli_screen::screen::Tab::Security) => &SECURITY,
            Some(mouli_screen::screen::Tab::Review) => &REVIEW,
        };
        let selected = d.selected_icon == Some(i);
        let ix = r.x + (r.w - 32) / 2;
        if selected {
            fill(c, Rect::new(ix - 3, r.y - 3, 38, 38), SELECT);
        }
        sprite(c, ix, r.y, art, &ICON_PALETTE, 2);
        let lw = text_width(spot.label, 1);
        let lx = r.x + (r.w - lw) / 2;
        let ly = r.y + 38;
        if selected {
            fill(c, Rect::new(lx - 3, ly - 2, lw + 6, 12), SELECT);
            text(c, lx, ly, spot.label, SELECT_TEXT, 1);
        } else {
            text(c, lx + 1, ly + 1, spot.label, BLACK, 1);
            text(c, lx, ly, spot.label, LIGHT, 1);
        }
    }
}

impl Desktop {
    /// Draws the whole desktop: the wallpaper, icons, windows, menu bar, any open menu or dialog,
    /// and the pointer.
    pub fn render(&self, shell: &Shell) -> Canvas {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut c = canvas(w, h, DESKTOP_A);
        for y in (0..h).step_by(2) {
            for x in (0..w).step_by(2) {
                c.pixels[y * w + x] = DESKTOP_B;
            }
        }
        draw_icons(&mut c, self);
        let front = self.wm.top().map(|win| win.id);
        for win in &self.wm.windows {
            draw_window(&mut c, win, Some(win.id) == front, shell);
        }
        self.menu.draw_bar(&mut c, &self.status);
        self.menu.draw_dropdown(&mut c, self.wm.top().is_some());
        if let Some(alert) = &self.alert {
            draw_alert(&mut c, self, alert);
        }
        let (px, py) = self.pointer;
        sprite(&mut c, px, py, &POINTER, &POINTER_PALETTE, 1);
        c
    }
}
