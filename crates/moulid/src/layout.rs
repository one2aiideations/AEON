//! Keyboard layouts: which character each physical key gives, plain, with Shift and with AltGr.
//!
//! The desktop reads the keyboard itself (evdev key codes name the physical key, not the letter),
//! so the layout is a table here. Five layouts: US, UK, German, French (AZERTY) and Spanish.
//!
//! Limits: dead keys (the accent keys on German, French and Spanish keyboards) type the plain
//! accent character at once instead of combining with the next letter; the euro sign is not
//! available (the built-in font has no glyph for it); only the main typing block is covered.

use std::sync::atomic::{AtomicU8, Ordering};

/// The physical keys covered, by evdev code, in the order the layout strings use: the key under
/// Esc, the digit row, the three letter rows, the key left of Enter, the extra key beside the
/// left Shift on ISO keyboards, the bottom row and the space bar.
const KEYS: [u16; 49] = [
    41, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, // 13
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, // 12
    30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, // 11
    43, 86, // 2
    44, 45, 46, 47, 48, 49, 50, 51, 52, 53, // 10
    57,
];

/// A layout: its name and three columns of 49 characters (`\0` for "nothing").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Short name used in settings and on the kernel command line.
    pub name: &'static str,
    /// What the person calls it.
    pub label: &'static str,
    plain: &'static str,
    shift: &'static str,
    altgr: &'static str,
}

/// US English.
pub const US: Layout = Layout {
    name: "us",
    label: "US English",
    plain: concat!(
        "`1234567890-=",
        "qwertyuiop[]",
        "asdfghjkl;'",
        "\\",
        "<",
        "zxcvbnm,./",
        " "
    ),
    shift: concat!(
        "~!@#$%^&*()_+",
        "QWERTYUIOP{}",
        "ASDFGHJKL:\"",
        "|",
        ">",
        "ZXCVBNM<>?",
        " "
    ),
    altgr: concat!(
        "\0\0\0\0\0\0\0\0\0\0\0\0\0",
        "\0\0\0\0\0\0\0\0\0\0\0\0",
        "\0\0\0\0\0\0\0\0\0\0\0",
        "\0",
        "\0",
        "\0\0\0\0\0\0\0\0\0\0",
        "\0"
    ),
};

/// UK English.
pub const GB: Layout = Layout {
    name: "gb",
    label: "UK English",
    plain: concat!(
        "`1234567890-=",
        "qwertyuiop[]",
        "asdfghjkl;'",
        "#",
        "\\",
        "zxcvbnm,./",
        " "
    ),
    shift: concat!(
        "¬!\"£$%^&*()_+",
        "QWERTYUIOP{}",
        "ASDFGHJKL:@",
        "~",
        "|",
        "ZXCVBNM<>?",
        " "
    ),
    altgr: concat!(
        "¦\0\0\0\0\0\0\0\0\0\0\0\0",
        "\0\0é\0\0\0úíó\0\0\0",
        "á\0\0\0\0\0\0\0\0\0\0",
        "\0",
        "\0",
        "\0\0\0\0\0\0\0\0\0\0",
        "\0"
    ),
};

/// German (QWERTZ).
pub const DE: Layout = Layout {
    name: "de",
    label: "German",
    plain: concat!(
        "^1234567890ß´",
        "qwertzuiopü+",
        "asdfghjklöä",
        "#",
        "<",
        "yxcvbnm,.-",
        " "
    ),
    shift: concat!(
        "°!\"§$%&/()=?`",
        "QWERTZUIOPÜ*",
        "ASDFGHJKLÖÄ",
        "'",
        ">",
        "YXCVBNM;:_",
        " "
    ),
    altgr: concat!(
        "\0\0²³\0\0\0{[]}\\\0",
        "@\0\0\0\0\0\0\0\0\0\0~",
        "\0\0\0\0\0\0\0\0\0\0\0",
        "\0",
        "|",
        "\0\0\0\0\0\0µ\0\0\0",
        "\0"
    ),
};

/// French (AZERTY).
pub const FR: Layout = Layout {
    name: "fr",
    label: "French",
    plain: concat!(
        "²&é\"'(-è_çà)=",
        "azertyuiop^$",
        "qsdfghjklmù",
        "*",
        "<",
        "wxcvbn,;:!",
        " "
    ),
    shift: concat!(
        "\01234567890°+",
        "AZERTYUIOP¨£",
        "QSDFGHJKLM%",
        "µ",
        ">",
        "WXCVBN?./§",
        " "
    ),
    altgr: concat!(
        "\0\0~#{[|`\\^@]}",
        "\0\0\0\0\0\0\0\0\0\0\0\0",
        "\0\0\0\0\0\0\0\0\0\0\0",
        "\0",
        "\0",
        "\0\0\0\0\0\0\0\0\0\0",
        "\0"
    ),
};

/// Spanish.
pub const ES: Layout = Layout {
    name: "es",
    label: "Spanish",
    plain: concat!(
        "º1234567890'¡",
        "qwertyuiop`+",
        "asdfghjklñ´",
        "ç",
        "<",
        "zxcvbnm,.-",
        " "
    ),
    shift: concat!(
        "ª!\"·$%&/()=?¿",
        "QWERTYUIOP^*",
        "ASDFGHJKLÑ¨",
        "Ç",
        ">",
        "ZXCVBNM;:_",
        " "
    ),
    altgr: concat!(
        "\\|@#~\0¬\0\0\0\0\0\0",
        "\0\0\0\0\0\0\0\0\0\0[]",
        "\0\0\0\0\0\0\0\0\0\0{",
        "}",
        "\0",
        "\0\0\0\0\0\0\0\0\0\0",
        "\0"
    ),
};

/// Every layout, in the order the Settings row offers them.
pub const ALL: [Layout; 5] = [US, GB, DE, FR, ES];

impl Layout {
    /// The layout called `name` (`uk` is accepted for `gb`), if there is one.
    pub fn by_name(name: &str) -> Option<Layout> {
        let n = name.trim().to_ascii_lowercase();
        let n = if n == "uk" { "gb" } else { n.as_str() };
        ALL.iter().copied().find(|l| l.name == n)
    }

    fn column(col: &str, code: u16) -> Option<char> {
        let i = KEYS.iter().position(|&k| k == code)?;
        col.chars().nth(i).filter(|&c| c != '\0')
    }

    /// The character for key `code`, or `None` when this layout has nothing there (or the key is
    /// not a typing key). `altgr` wins over `shift`; with both held, an accented letter on AltGr
    /// comes out capital.
    pub fn char_for(&self, code: u16, shift: bool, altgr: bool) -> Option<char> {
        if altgr {
            if let Some(c) = Self::column(self.altgr, code) {
                return Some(if shift {
                    let mut up = c.to_uppercase();
                    match (up.next(), up.next()) {
                        // Only when the capital is in the font too (the capital of µ is Greek).
                        (Some(u), None) if u <= '\u{FF}' => u,
                        _ => c,
                    }
                } else {
                    c
                });
            }
        }
        Self::column(if shift { self.shift } else { self.plain }, code)
    }

    /// Whether Caps Lock should act on this key: it does for keys that carry a letter.
    pub fn is_letter(&self, code: u16) -> bool {
        Self::column(self.plain, code).is_some_and(char::is_alphabetic)
    }
}

/// The layout the whole desktop uses (an index into [`ALL`]); changing it takes effect on the
/// next key pressed.
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The layout named by `mouli.keymap=xx` on the kernel command line, if there is one.
pub fn from_cmdline(cmdline: &str) -> Option<Layout> {
    cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("mouli.keymap="))
        .and_then(Layout::by_name)
}

/// The layout in use.
pub fn current() -> Layout {
    ALL.get(usize::from(CURRENT.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or(US)
}

/// Switches layout. Returns false for a name that is not known.
pub fn set_current(name: &str) -> bool {
    match Layout::by_name(name) {
        Some(l) => {
            let i = ALL.iter().position(|x| x.name == l.name).unwrap_or(0);
            CURRENT.store(u8::try_from(i).unwrap_or(0), Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Tests that change the process-wide layout take this first, so they do not overlap.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_column_has_one_entry_per_key() {
        for l in ALL {
            for (what, col) in [("plain", l.plain), ("shift", l.shift), ("altgr", l.altgr)] {
                assert_eq!(col.chars().count(), KEYS.len(), "{} {what}", l.name);
            }
        }
    }

    #[test]
    fn every_layout_can_type_all_of_ascii_that_shells_and_code_need() {
        let want: Vec<char> = (0x20u8..0x7F).map(char::from).collect();
        for l in ALL {
            let mut typed = std::collections::BTreeSet::new();
            for &code in &KEYS {
                for (shift, altgr) in [(false, false), (true, false), (false, true)] {
                    typed.extend(l.char_for(code, shift, altgr));
                }
            }
            let missing: String = want.iter().filter(|c| !typed.contains(c)).collect();
            assert!(missing.is_empty(), "{} cannot type {missing:?}", l.name);
        }
    }

    #[test]
    fn spot_checks_of_the_places_where_layouts_differ() {
        // Evdev codes: 44 is the key right of left Shift's neighbour (Z on US), 16 is Q on US.
        assert_eq!(DE.char_for(44, false, false), Some('y'));
        assert_eq!(DE.char_for(21, false, false), Some('z'));
        assert_eq!(FR.char_for(16, false, false), Some('a'));
        assert_eq!(FR.char_for(30, false, false), Some('q'));
        assert_eq!(FR.char_for(39, false, false), Some('m'));
        assert_eq!(FR.char_for(2, false, false), Some('&'));
        assert_eq!(FR.char_for(2, true, false), Some('1'));
        assert_eq!(GB.char_for(4, true, false), Some('£'));
        assert_eq!(GB.char_for(3, true, false), Some('"'));
        assert_eq!(US.char_for(3, true, false), Some('@'));
        assert_eq!(DE.char_for(16, false, true), Some('@'));
        assert_eq!(ES.char_for(3, false, true), Some('@'));
        assert_eq!(DE.char_for(86, false, true), Some('|'));
        assert_eq!(DE.char_for(39, false, false), Some('ö'));
        assert_eq!(ES.char_for(39, true, false), Some('Ñ'));
        assert_eq!(GB.char_for(18, false, true), Some('é'));
        assert_eq!(GB.char_for(18, true, true), Some('É'));
    }

    #[test]
    fn caps_lock_applies_to_letters_only() {
        assert!(US.is_letter(30));
        assert!(!US.is_letter(2));
        assert!(!FR.is_letter(2), "the French digit row types symbols");
        assert!(DE.is_letter(39), "ö is a letter");
        assert!(!US.is_letter(999));
    }

    #[test]
    fn the_kernel_command_line_can_name_a_layout() {
        assert_eq!(
            from_cmdline("quiet mouli.keymap=de ro").map(|l| l.name),
            Some("de")
        );
        assert_eq!(from_cmdline("mouli.keymap=uk").map(|l| l.name), Some("gb"));
        assert!(from_cmdline("mouli.keymap=xx").is_none());
        assert!(from_cmdline("quiet").is_none());
        assert!(from_cmdline("xmouli.keymap=de").is_none());
    }

    #[test]
    fn names_resolve_and_the_current_layout_can_change() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(Layout::by_name("DE").map(|l| l.name), Some("de"));
        assert_eq!(Layout::by_name(" uk ").map(|l| l.name), Some("gb"));
        assert!(Layout::by_name("klingon").is_none());
        assert!(!set_current("klingon"));
        assert!(set_current("fr"));
        assert_eq!(current().name, "fr");
        assert!(set_current("us"));
        assert_eq!(current().name, "us");
    }

    #[test]
    fn every_character_is_in_the_built_in_font() {
        // The font covers printable ASCII and Latin-1 (U+00A0 to U+00FF).
        for l in ALL {
            for &code in &KEYS {
                for (shift, altgr) in [(false, false), (true, false), (false, true), (true, true)] {
                    if let Some(c) = l.char_for(code, shift, altgr) {
                        let ok = (' '..='~').contains(&c) || ('\u{A0}'..='\u{FF}').contains(&c);
                        assert!(ok, "{}: {c:?} is not in the font", l.name);
                    }
                }
            }
        }
    }
}
