//! Keyboard input: turning the bytes a terminal sends into [`Key`] values.

/// One key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character.
    Char(char),
    /// Enter / Return.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Shift-Tab.
    BackTab,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A function key, F1 to F12.
    F(u8),
    /// Control plus a letter (`Ctrl('c')`).
    Ctrl(char),
}

/// Incremental parser for terminal key bytes, including escape sequences, UTF-8 and the Linux
/// console's own function-key codes. Bytes can arrive in any split; an incomplete sequence is kept
/// until more bytes arrive or [`KeyParser::flush`] is called.
#[derive(Debug, Default)]
pub struct KeyParser {
    buf: Vec<u8>,
    /// True while skipping the rest of an escape sequence that grew too long.
    discarding: bool,
}

/// Longest escape sequence we wait for before giving up on it.
const MAX_SEQ: usize = 16;

impl KeyParser {
    /// An empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes and returns the keys they completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.buf.extend_from_slice(bytes);
        self.drain(false)
    }

    /// Call when no more bytes are coming soon (a short pause after input): a lone Escape that
    /// was being held back is now a real Escape key, and incomplete UTF-8 is dropped.
    pub fn flush(&mut self) -> Vec<Key> {
        self.drain(true)
    }

    fn drain(&mut self, flush: bool) -> Vec<Key> {
        let mut out = Vec::new();
        loop {
            if self.discarding {
                // Drop everything up to and including the sequence's final byte.
                match self.buf.iter().position(|b| (0x40..=0x7E).contains(b)) {
                    Some(end) => {
                        self.buf.drain(..=end);
                        self.discarding = false;
                    }
                    None => {
                        self.buf.clear();
                        break;
                    }
                }
                continue;
            }
            match parse_one(&self.buf, flush) {
                Parsed::Key(k, used) => {
                    out.push(k);
                    self.buf.drain(..used);
                }
                Parsed::Skip(used) => {
                    self.buf.drain(..used);
                }
                Parsed::Overflow(used) => {
                    self.buf.drain(..used);
                    self.discarding = true;
                }
                Parsed::Need => break,
            }
        }
        out
    }
}

enum Parsed {
    Key(Key, usize),
    Skip(usize),
    /// An escape sequence grew past the limit: drop what we have and ignore the rest of it.
    Overflow(usize),
    Need,
}

fn parse_one(b: &[u8], flush: bool) -> Parsed {
    let Some(&first) = b.first() else {
        return Parsed::Need;
    };
    match first {
        0x1B => parse_escape(b, flush),
        b'\r' | b'\n' => Parsed::Key(Key::Enter, 1),
        b'\t' => Parsed::Key(Key::Tab, 1),
        0x7F | 0x08 => Parsed::Key(Key::Backspace, 1),
        0x01..=0x1A => Parsed::Key(Key::Ctrl((b'a' + first - 1) as char), 1),
        0x20..=0x7E => Parsed::Key(Key::Char(first as char), 1),
        0x00 | 0x1C..=0x1F => Parsed::Skip(1),
        _ => parse_utf8(b, flush),
    }
}

fn parse_utf8(b: &[u8], flush: bool) -> Parsed {
    let need = match b.first() {
        Some(0xC2..=0xDF) => 2,
        Some(0xE0..=0xEF) => 3,
        Some(0xF0..=0xF4) => 4,
        _ => return Parsed::Skip(1),
    };
    if b.len() < need {
        return if flush {
            Parsed::Skip(b.len())
        } else {
            Parsed::Need
        };
    }
    match std::str::from_utf8(&b[..need])
        .ok()
        .and_then(|s| s.chars().next())
    {
        Some(c) if !c.is_control() => Parsed::Key(Key::Char(c), need),
        _ => Parsed::Skip(1),
    }
}

fn parse_escape(b: &[u8], flush: bool) -> Parsed {
    let Some(&second) = b.get(1) else {
        return if flush {
            Parsed::Key(Key::Esc, 1)
        } else {
            Parsed::Need
        };
    };
    match second {
        b'[' => parse_csi(b, flush),
        b'O' => match b.get(2) {
            Some(b'P') => Parsed::Key(Key::F(1), 3),
            Some(b'Q') => Parsed::Key(Key::F(2), 3),
            Some(b'R') => Parsed::Key(Key::F(3), 3),
            Some(b'S') => Parsed::Key(Key::F(4), 3),
            Some(b'H') => Parsed::Key(Key::Home, 3),
            Some(b'F') => Parsed::Key(Key::End, 3),
            Some(_) => Parsed::Skip(3),
            None => {
                if flush {
                    Parsed::Skip(2)
                } else {
                    Parsed::Need
                }
            }
        },
        // Escape followed by something else: a real Escape, then that byte on its own.
        _ => Parsed::Key(Key::Esc, 1),
    }
}

fn parse_csi(b: &[u8], flush: bool) -> Parsed {
    // The Linux console sends F1..F5 as ESC [ [ A..E.
    if b.get(2) == Some(&b'[') {
        return match b.get(3) {
            Some(c @ b'A'..=b'E') => Parsed::Key(Key::F(c - b'A' + 1), 4),
            Some(_) => Parsed::Skip(4),
            None => {
                if flush {
                    Parsed::Skip(3)
                } else {
                    Parsed::Need
                }
            }
        };
    }
    let mut i = 2;
    while let Some(&c) = b.get(i) {
        if (0x40..=0x7E).contains(&c) {
            let params = std::str::from_utf8(&b[2..i]).unwrap_or("");
            return match csi_key(params, c) {
                Some(k) => Parsed::Key(k, i + 1),
                None => Parsed::Skip(i + 1),
            };
        }
        i += 1;
        if i > MAX_SEQ {
            return Parsed::Overflow(i);
        }
    }
    if flush {
        Parsed::Skip(b.len())
    } else {
        Parsed::Need
    }
}

fn csi_key(params: &str, final_byte: u8) -> Option<Key> {
    let first: u32 = params
        .split(';')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    Some(match final_byte {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'Z' => Key::BackTab,
        b'~' => match first {
            1 | 7 => Key::Home,
            4 | 8 => Key::End,
            3 => Key::Delete,
            5 => Key::PageUp,
            6 => Key::PageDown,
            11..=15 => Key::F((first - 10) as u8),
            17..=21 => Key::F((first - 11) as u8),
            23 | 24 => Key::F((first - 12) as u8),
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut p = KeyParser::new();
        let mut k = p.feed(bytes);
        k.extend(p.flush());
        k
    }

    #[test]
    fn plain_keys() {
        assert_eq!(
            keys(b"hi\r"),
            vec![Key::Char('h'), Key::Char('i'), Key::Enter]
        );
        assert_eq!(
            keys(b"\t\x7f\x08"),
            vec![Key::Tab, Key::Backspace, Key::Backspace]
        );
        assert_eq!(
            keys(b"\x03\x01\x1a"),
            vec![Key::Ctrl('c'), Key::Ctrl('a'), Key::Ctrl('z')]
        );
        assert_eq!(keys(b" ~"), vec![Key::Char(' '), Key::Char('~')]);
    }

    #[test]
    fn arrows_and_navigation() {
        assert_eq!(
            keys(b"\x1b[A\x1b[B\x1b[C\x1b[D"),
            vec![Key::Up, Key::Down, Key::Right, Key::Left]
        );
        assert_eq!(
            keys(b"\x1b[H\x1b[F\x1b[3~\x1b[5~\x1b[6~\x1b[Z"),
            vec![
                Key::Home,
                Key::End,
                Key::Delete,
                Key::PageUp,
                Key::PageDown,
                Key::BackTab
            ]
        );
        assert_eq!(
            keys(b"\x1b[1~\x1b[4~\x1b[7~\x1b[8~"),
            vec![Key::Home, Key::End, Key::Home, Key::End]
        );
        assert_eq!(
            keys(b"\x1b[1;5C"),
            vec![Key::Right],
            "modifiers are ignored but the key survives"
        );
        assert_eq!(keys(b"\x1bOH\x1bOF"), vec![Key::Home, Key::End]);
    }

    #[test]
    fn function_keys_in_xterm_and_linux_console_forms() {
        assert_eq!(
            keys(b"\x1bOP\x1bOQ\x1bOR\x1bOS"),
            (1..=4).map(Key::F).collect::<Vec<_>>()
        );
        assert_eq!(
            keys(b"\x1b[[A\x1b[[B\x1b[[C\x1b[[D\x1b[[E"),
            (1..=5).map(Key::F).collect::<Vec<_>>()
        );
        assert_eq!(
            keys(b"\x1b[15~\x1b[17~\x1b[18~\x1b[19~\x1b[20~\x1b[21~\x1b[23~\x1b[24~"),
            vec![
                Key::F(5),
                Key::F(6),
                Key::F(7),
                Key::F(8),
                Key::F(9),
                Key::F(10),
                Key::F(11),
                Key::F(12)
            ]
        );
    }

    #[test]
    fn utf8_characters() {
        assert_eq!(
            keys("é€😀".as_bytes()),
            vec![Key::Char('é'), Key::Char('€'), Key::Char('😀')]
        );
    }

    #[test]
    fn a_lone_escape_waits_for_a_flush() {
        let mut p = KeyParser::new();
        assert!(
            p.feed(b"\x1b").is_empty(),
            "might be the start of a sequence"
        );
        assert_eq!(p.flush(), vec![Key::Esc]);
        // ESC followed by a letter is Escape then the letter.
        assert_eq!(keys(b"\x1bx"), vec![Key::Esc, Key::Char('x')]);
    }

    #[test]
    fn sequences_may_arrive_in_pieces() {
        let mut p = KeyParser::new();
        assert!(p.feed(b"\x1b").is_empty());
        assert!(p.feed(b"[").is_empty());
        assert_eq!(p.feed(b"A"), vec![Key::Up]);
        assert!(p.feed(&"€".as_bytes()[..2]).is_empty());
        assert_eq!(p.feed(&"€".as_bytes()[2..]), vec![Key::Char('€')]);
    }

    #[test]
    fn garbage_is_skipped_never_stuck() {
        assert_eq!(keys(b"\x00\x1c\xff\x80a"), vec![Key::Char('a')]);
        // An unknown escape sequence is dropped whole, and typing continues.
        assert_eq!(keys(b"\x1b[99;99qZ"), vec![Key::Char('Z')]);
        // A runaway escape sequence cannot block input forever.
        let mut junk = vec![0x1B, b'['];
        junk.extend(std::iter::repeat_n(b'1', 40));
        junk.extend_from_slice(b"xok");
        // The whole runaway sequence, including its final byte `x`, is swallowed.
        assert_eq!(keys(&junk), vec![Key::Char('o'), Key::Char('k')]);
        // Truncated UTF-8 is dropped on flush.
        assert_eq!(keys(&"€".as_bytes()[..2]), Vec::<Key>::new());
    }

    #[test]
    fn typing_after_an_escape_sequence_in_one_burst() {
        assert_eq!(
            keys(b"\x1b[Aok"),
            vec![Key::Up, Key::Char('o'), Key::Char('k')]
        );
    }
}
