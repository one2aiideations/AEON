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

/// Icon palette: `k` black, `w` white, `g` light grey, `d` dark grey, `b` blue, `r` red,
/// `y` yellow, `o` green.
pub const ICON_PALETTE: [(char, u32); 8] = [
    ('k', 0x000000),
    ('w', 0xFFFFFF),
    ('g', 0xD4D4D4),
    ('d', 0x808080),
    ('b', 0x3F69B5),
    ('r', 0xD0443E),
    ('y', 0xF2C94C),
    ('o', 0x3DAA6B),
];

/// 16 by 16 icon pictures, drawn at twice their size.
pub type Icon = [&'static str; 16];

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
