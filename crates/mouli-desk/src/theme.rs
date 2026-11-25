//! Colours from Google's Material Design dark theme (material.io): a dark surface that rises in
//! lightness with elevation, white text at 87% and 60%, and Blue 200 / Blue 700 as the primary.
//! They match the screens drawn inside the windows (`mouli_screen::grid::theme`).

/// The desktop behind the windows: Blue Grey 900.
pub const WALLPAPER: u32 = 0x263238;
/// The base surface (menu bar, hint strip, windows behind): the dark surface plus a 1dp overlay.
pub const SURFACE: u32 = 0x1E1E1E;
/// A raised surface (front window title bar, menus, dialogs): the 8dp overlay.
pub const SURFACE_HIGH: u32 = 0x2C2C2C;
/// Thin edges between surfaces.
pub const OUTLINE: u32 = 0x3F3F3F;
/// Edge of the front window.
pub const OUTLINE_ACTIVE: u32 = 0x5A5A5A;
/// Text and icons on a surface: white at 87%.
pub const ON_SURFACE: u32 = 0xE0E0E0;
/// Secondary text: white at about 60%.
pub const ON_SURFACE_DIM: u32 = 0x9E9E9E;
/// Disabled text: white at 38%.
pub const DISABLED: u32 = 0x6C6C6C;
/// The primary colour on dark surfaces: Blue 200.
pub const PRIMARY: u32 = 0x90CAF9;
/// A filled primary control (the OK button, a selected icon): Blue 700, white text on it.
pub const PRIMARY_FILL: u32 = 0x1976D2;
/// Text on a filled primary control.
pub const ON_PRIMARY: u32 = 0xFFFFFF;
/// A hovered or selected menu item: Blue 400 at 24% over the surface.
pub const HOVER: u32 = 0x1E3549;
/// Pure black, for shadows and the pointer outline.
pub const BLACK: u32 = 0x000000;
