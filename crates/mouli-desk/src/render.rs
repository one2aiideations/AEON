//! Drawing the desktop in Material Design style: flat dark surfaces that get lighter as they
//! rise, soft shadows, Roboto text.

use crate::art::*;
use crate::desktop::{Alert, Desktop};
use crate::geom::Rect;
use crate::paint::*;
use crate::theme::*;
use crate::wm::{Window, STATUS_H, TITLE_H};
use mouli_screen::grid::Grid;
use mouli_screen::raster::{render_text, Canvas};
use mouli_screen::screen::Tab;
use mouli_screen::shell::Shell;
use mouli_screen::typeface::{ui, ui_medium, ui_small};

/// A filled primary button with a centred label.
fn button(c: &mut Canvas, r: Rect, label: &str) {
    fill(c, r, PRIMARY_FILL);
    let face = ui_medium();
    let w = text_width(face, label);
    text(
        c,
        face,
        r.x + (r.w - w) / 2,
        r.y + (r.h - face.line_height) / 2,
        label,
        ON_PRIMARY,
    );
}

fn draw_window(c: &mut Canvas, w: &Window, active: bool, shell: &Shell) {
    let o = w.outer();
    shadow(c, o);
    fill(c, o, SURFACE);
    outline(c, o, if active { OUTLINE_ACTIVE } else { OUTLINE });

    // Title bar: raised when it is the front window.
    let t = w.title_bar();
    fill(c, t, if active { SURFACE_HIGH } else { SURFACE });
    hline(c, t.x, t.bottom(), t.w, OUTLINE);
    let face = ui_medium();
    let title = w.tab.label();
    let tw = text_width(face, title);
    let colour = if active { ON_SURFACE } else { ON_SURFACE_DIM };
    text(
        c,
        face,
        t.x + (t.w - tw) / 2,
        t.y + (TITLE_H - face.line_height) / 2,
        title,
        colour,
    );
    if active {
        // Close: a cross. Zoom: a square, or two when the window is already enlarged.
        let b = w.close_box();
        let m = 4;
        line(
            c,
            (b.x + m, b.y + m),
            (b.right() - m - 1, b.bottom() - m - 1),
            ON_SURFACE,
        );
        line(
            c,
            (b.right() - m - 1, b.y + m),
            (b.x + m, b.bottom() - m - 1),
            ON_SURFACE,
        );
        let z = w.zoom_box();
        if w.zoomed {
            outline(c, Rect::new(z.x + 3, z.y + 5, 8, 8), ON_SURFACE);
            hline(c, z.x + 6, z.y + 3, 7, ON_SURFACE);
            vline(c, z.x + 12, z.y + 3, 7, ON_SURFACE);
        } else {
            outline(c, Rect::new(z.x + 3, z.y + 3, 10, 10), ON_SURFACE);
            hline(c, z.x + 3, z.y + 4, 10, ON_SURFACE);
        }
    }

    // The screen inside the window, in Roboto Mono.
    let k = w.content();
    let mut grid = Grid::new(w.cols as usize, w.rows as usize);
    let area = grid.area();
    shell.screen(w.tab).draw(&mut grid, area);
    blit(c, &render_text(&grid), k.x, k.y, k);

    // The strip under it: key hints, and the grow handle.
    let s = w.status_strip();
    fill(c, s, SURFACE);
    hline(c, s.x, s.y, s.w, OUTLINE);
    let small = ui_small();
    let hints = shell.screen(w.tab).hints();
    let room = s.w - STATUS_H - 16;
    let mut shown = String::new();
    for ch in hints.chars() {
        if text_width(small, &shown) + small.glyph(ch).advance > room {
            break;
        }
        shown.push(ch);
    }
    text(
        c,
        small,
        s.x + 12,
        s.y + (STATUS_H - small.line_height) / 2,
        &shown,
        ON_SURFACE_DIM,
    );
    let g = w.grow_box();
    for i in 0..3 {
        let off = 5 + i * 4;
        line(
            c,
            (g.right() - 3, g.bottom() - off),
            (g.right() - off, g.bottom() - 3),
            ON_SURFACE_DIM,
        );
    }
}

fn draw_alert(c: &mut Canvas, d: &Desktop, alert: &Alert) {
    let (r, ok) = d.alert_rects(alert);
    shadow(c, r);
    fill(c, r, SURFACE_HIGH);
    outline(c, r, OUTLINE_ACTIVE);
    let (head, body) = (ui_medium(), ui());
    for (i, line) in alert.lines.iter().enumerate() {
        let face = if i == 0 && alert.lines.len() > 1 {
            head
        } else {
            body
        };
        let colour = if i == 0 && alert.lines.len() > 1 {
            PRIMARY
        } else {
            ON_SURFACE
        };
        text(c, face, r.x + 28, r.y + 24 + i as i32 * 22, line, colour);
    }
    button(c, ok, "OK");
}

fn draw_icons(c: &mut Canvas, d: &Desktop) {
    let face = ui_small();
    for (i, spot) in d.icons.iter().enumerate() {
        let r = d.icon_rect(i);
        let art: &Icon = match spot.tab {
            None => &DISK,
            Some(Tab::Chat) => &CHAT,
            Some(Tab::Models) => &MODELS,
            Some(Tab::Approvals) => &APPROVALS,
            Some(Tab::Audit) => &AUDIT,
            Some(Tab::Security) => &SECURITY,
            Some(Tab::Review) => &REVIEW,
            Some(Tab::Sheet) => &SHEET,
            Some(Tab::Studio) => &STUDIO,
            Some(Tab::Editor) => &CODE,
        };
        let selected = d.selected_icon == Some(i);
        let ix = r.x + (r.w - 32) / 2;
        if selected {
            fill_alpha(c, Rect::new(ix - 5, r.y - 5, 42, 42), PRIMARY, 60);
        }
        sprite(c, ix, r.y, art, &ICON_PALETTE, 2);
        let lw = text_width(face, spot.label);
        let lx = r.x + (r.w - lw) / 2;
        let ly = r.y + 40;
        if selected {
            fill(
                c,
                Rect::new(lx - 5, ly - 1, lw + 10, face.line_height + 2),
                PRIMARY_FILL,
            );
            text(c, face, lx, ly, spot.label, ON_PRIMARY);
        } else {
            text(c, face, lx + 1, ly + 1, spot.label, BLACK);
            text(c, face, lx, ly, spot.label, ON_SURFACE);
        }
    }
}

impl Desktop {
    /// Draws the whole desktop: the wallpaper, icons, windows, menu bar, any open menu or dialog,
    /// and the pointer.
    pub fn render(&self, shell: &Shell) -> Canvas {
        let mut c = canvas(self.width as usize, self.height as usize, WALLPAPER);
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
