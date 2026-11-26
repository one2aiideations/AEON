//! Small pictures: the pointer and the desktop icons, as text so they can be read and edited.

/// Pointer palette: `k` black outline, `w` white fill.
pub const POINTER_PALETTE: [(char, u32); 2] = [('k', 0x000000), ('w', 0xFFFFFF)];

/// The arrow pointer. Its hot spot (the tip) is the top-left pixel.
pub const POINTER: [&str; 17] = [
    "k..........",
    "kk.........",
    "kwk........",
    "kwwk.......",
    "kwwwk......",
    "kwwwwk.....",
    "kwwwwwk....",
    "kwwwwwwk...",
    "kwwwwwwwk..",
    "kwwwwwwwwk.",
    "kwwwwwkkkkk",
    "kwwkwwk....",
    "kwk.kwwk...",
    "kk..kwwk...",
    "k....kwwk..",
    ".....kwwk..",
    "......kk...",
];

/// Icon palette, the Material 500 colours: `k` Grey 900, `w` Grey 50, `g` Grey 400, `d` Grey 600,
/// `b` Blue, `r` Red, `y` Amber, `o` Green.
pub const ICON_PALETTE: [(char, u32); 8] = [
    ('k', 0x212121),
    ('w', 0xFAFAFA),
    ('g', 0xBDBDBD),
    ('d', 0x757575),
    ('b', 0x2196F3),
    ('r', 0xF44336),
    ('y', 0xFFC107),
    ('o', 0x4CAF50),
];

/// 16 by 16 icon pictures, drawn at twice their size.
pub type Icon = [&'static str; 16];

/// The browser: a globe.
pub const BROWSER: Icon = [
    "................",
    "....kkkkkkkk....",
    "..kkbbbwbbbbkk..",
    ".kbbbobwbbbobbk.",
    ".kbooobwbbooobk.",
    ".kbooobwbbbobbk.",
    "kbbbbbbwbbbbbbbk",
    "kwwwwwwwwwwwwwwk",
    "kbbbbbbwbbbbbbbk",
    ".kbbbooowbbbbbk.",
    ".kbbbooowbbbbbk.",
    ".kbbbbobwbbbbbk.",
    "..kkbbbwbbbbkk..",
    "....kkkkkkkk....",
    "................",
    "................",
];

/// The hard disk.
pub const DISK: Icon = [
    "................",
    "................",
    "................",
    "..kkkkkkkkkkkk..",
    ".kwwwwwwwwwwwwk.",
    ".kwggggggggggdk.",
    ".kwggggggggggdk.",
    ".kwddddddddddgk.",
    ".kwggggggoyggdk.",
    ".kwggggggggggdk.",
    ".kddddddddddddk.",
    "..kkkkkkkkkkkk..",
    "................",
    "................",
    "................",
    "................",
];

/// Chat: a speech bubble.
pub const CHAT: Icon = [
    "................",
    "..kkkkkkkkkkkk..",
    ".kwwwwwwwwwwwwk.",
    ".kwbbbbbbbbbbwk.",
    ".kwwwwwwwwwwwwk.",
    ".kwbbbbbbbwwwwk.",
    ".kwwwwwwwwwwwwk.",
    ".kwbbbbbbbbbwwk.",
    ".kwwwwwwwwwwwwk.",
    "..kkkkkkwwkkkk..",
    "........kwwk....",
    ".........kwk....",
    "..........kk....",
    "................",
    "................",
    "................",
];

/// Models: a chip.
pub const MODELS: Icon = [
    "................",
    "...k..k..k..k...",
    "...k..k..k..k...",
    "..kkkkkkkkkkkk..",
    "..kddddddddddk..",
    "kkkdwwwwwwwwdkkk",
    "..kdwbbbbbbwdk..",
    "kkkdwbwwwwbwdkkk",
    "..kdwbwwwwbwdk..",
    "kkkdwbbbbbbwdkkk",
    "..kdwwwwwwwwdk..",
    "..kddddddddddk..",
    "..kkkkkkkkkkkk..",
    "...k..k..k..k...",
    "...k..k..k..k...",
    "................",
];

/// Approvals: a checklist.
pub const APPROVALS: Icon = [
    "................",
    "..kkkkkkkkkkkk..",
    "..kwwwwwwwwwwk..",
    "..kwowwkkkkwwk..",
    "..kooowwwwwwwk..",
    "..kwowwkkkkwwk..",
    "..kwwwwwwwwwwk..",
    "..kwowwkkkkwwk..",
    "..kooowwwwwwwk..",
    "..kwowwkkkkwwk..",
    "..kwwwwwwwwwwk..",
    "..kkkkkkkkkkkk..",
    "................",
    "................",
    "................",
    "................",
];

/// Audit: a scroll of records.
pub const AUDIT: Icon = [
    "................",
    "...kkkkkkkkkk...",
    "..kwwwwwwwwwwk..",
    "..kwdddddddwwk..",
    "..kwwwwwwwwwwk..",
    "..kwdddddddwwk..",
    "..kwwwwwwwwwwk..",
    "..kwdddddwwwwk..",
    "..kwwwwwwwwwwk..",
    "..kwdddddddwwk..",
    "...kwwwwwwwwwk..",
    "....kkkkkkkkkk..",
    "................",
    "................",
    "................",
    "................",
];

/// Security: a shield.
pub const SECURITY: Icon = [
    "................",
    "...kkkkkkkkkk...",
    "..kbbbbwbbbbbk..",
    "..kbbbbwbbbbbk..",
    "..kbbbbwbbbbbk..",
    "..kwwwwwwwwwwk..",
    "..kbbbbwbbbbbk..",
    "..kbbbbwbbbbbk..",
    "...kbbbwbbbbk...",
    "....kbbwbbbk....",
    ".....kbwbbk.....",
    "......kbbk......",
    ".......kk.......",
    "................",
    "................",
    "................",
];

/// Code review: a document with changed lines.
pub const REVIEW: Icon = [
    "................",
    "..kkkkkkkkkk....",
    "..kwwwwwwwwkk...",
    "..kwwwwwwwwkwk..",
    "..kwrrrrrwwkkk..",
    "..kwwwwwwwwwwk..",
    "..kwooooowwwwk..",
    "..kwwwwwwwwwwk..",
    "..kwooooooowwk..",
    "..kwwwwwwwwwwk..",
    "..kwrrrwwwwwwk..",
    "..kwwwwwwwwwwk..",
    "..kkkkkkkkkkkk..",
    "................",
    "................",
    "................",
];

/// Sheet: a table with a header row.
pub const SHEET: Icon = [
    "................",
    "................",
    ".kkkkkkkkkkkkkk.",
    ".koookoookooook.",
    ".koookoookooook.",
    ".kkkkkkkkkkkkkk.",
    ".kwwwkwwwkwwwwk.",
    ".kwwwkwwwkwwwwk.",
    ".kkkkkkkkkkkkkk.",
    ".kwwwkwwwkwwwwk.",
    ".kwwwkwwwkwwwwk.",
    ".kkkkkkkkkkkkkk.",
    ".kwwwkwwwkwwwwk.",
    ".kkkkkkkkkkkkkk.",
    "................",
    "................",
];

/// Studio: a page being written with a pencil.
pub const STUDIO: Icon = [
    "................",
    "..kkkkkkkkkk....",
    "..kwwwwwwwwk....",
    "..kwbbbbbbwk....",
    "..kwbbbbbbwk....",
    "..kwwwwwwwwk....",
    "..kwddddddwk....",
    "..kwwwwwwwwk..k.",
    "..kwddddddwk.y..",
    "..kwwwwwwwwky...",
    "..kwddddddwy....",
    "..kwwwwwwwyk....",
    "..kwdddwwywk....",
    "..kwwwwwkwwk....",
    "..kkkkkkkkkk....",
    "................",
];

/// Code: an editor window with a title bar and lines of source.
pub const CODE: Icon = [
    "................",
    ".kkkkkkkkkkkkkk.",
    ".kbrybbbbbbbbbk.",
    ".kkkkkkkkkkkkkk.",
    ".kooooddddddddk.",
    ".kdddyyyyyydddk.",
    ".kdddwwwwwwwwdk.",
    ".kdddwwwwdddddk.",
    ".kdddyyyddyyydk.",
    ".kddddddddooook.",
    ".kdddwwwwwwdddk.",
    ".kddddddddddddk.",
    ".kdddyyyyyydddk.",
    ".kkkkkkkkkkkkkk.",
    "................",
    "................",
];

/// Player: a screen with a play triangle and a progress bar.
pub const PLAYER: Icon = [
    "................",
    ".kkkkkkkkkkkkkk.",
    ".kddddddddddddk.",
    ".kddddddddddddk.",
    ".kddkkddddddddk.",
    ".kddkrkddddddyk.",
    ".kddkrrkdddddyk.",
    ".kddkrkddddddyk.",
    ".kddkkddddddddk.",
    ".kddddddddddddk.",
    ".kdbbbbbbbbbddk.",
    ".kdbbbbbbbbbddk.",
    ".kddddddddddddk.",
    ".kkkkkkkkkkkkkk.",
    "................",
    "................",
];
