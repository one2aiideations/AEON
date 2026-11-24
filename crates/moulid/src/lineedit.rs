//! A pure state machine for editing a single line of text on a Linux text
//! console (or any byte-oriented terminal).
//!
//! The machine consumes bytes one at a time via [`Editor::feed`] and returns
//! [`Action`] values telling a caller what to do; it performs no I/O itself.
//! The payload of [`Action::Redraw`] is the exact byte sequence to write to
//! the terminal: carriage return, the prompt, the visible text, `\x1b[K`
//! (clear to end of line) and finally `\x1b[<n>D` to move the cursor left by
//! `<n>` characters when it is not at the end of the text.
//!
//! Recognised control keys: Enter (`\r`/`\n`), Ctrl-C, Ctrl-D, Ctrl-U,
//! Ctrl-W, Ctrl-A/Ctrl-E, Backspace (`0x7f`/`0x08`), Delete (`ESC [ 3 ~`).
//! Recognised escape sequences: `ESC [ H` / `ESC [ 1 ~` / `ESC O H` (home),
//! `ESC [ F` / `ESC [ 4 ~` / `ESC O F` (end), `ESC [ C` / `ESC [ D`
//! (right/left, also in SS3 form), `ESC [ A` / `ESC [ B` (history up/down).
//! Any other sequence is swallowed safely: the parser state resets after any
//! final byte in `0x40..=0x7e` or after 8 bytes. A lone `ESC` followed by a
//! normal letter is swallowed as well. Malformed UTF-8 (including overlong
//! sequences) is discarded with [`Action::Bell`].
//!
//! A "secret" mode ([`Editor::secret`]) echoes `*` for every character and
//! never records or recalls history.

/// The outcome of feeding one byte to the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do; the byte was consumed silently.
    None,
    /// Bytes the caller should write to the terminal to bring the display in
    /// line with the buffer: `"\r"`, the prompt, the visible text, `"\x1b[K"`
    /// and, when the cursor is not at the end, `"\x1b[<n>D"` to move it back.
    Redraw(String),
    /// The user pressed Enter; carries the final line contents.
    Done(String),
    /// The user pressed Ctrl-C.
    Cancelled,
    /// The user pressed Ctrl-D on an empty line.
    EndOfInput,
    /// The attempted operation is impossible right now (e.g. the line is full,
    /// or there is nothing to delete); ring the terminal bell.
    Bell,
}

/// How many history entries are kept at most.
const MAX_HISTORY: usize = 50;
/// Maximum length of an escape sequence, counting the leading `ESC`.
const MAX_ESC_LEN: usize = 8;
/// Maximum number of parameter bytes kept inside a CSI sequence.
const MAX_PARAMS: usize = 8;

/// State of the escape-sequence parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseState {
    /// Ordinary input.
    Normal,
    /// Received `ESC`; expecting `[`, `O`, or a final byte.
    Esc,
    /// Inside a CSI sequence (`ESC [`).
    Csi,
    /// Inside an SS3 sequence (`ESC O`).
    Ss3,
}

/// A single-line editor.
///
/// Bytes are fed one at a time; printable ASCII and UTF-8 multi-byte
/// characters are inserted at the cursor. The buffer is reported as finished
/// via [`Action::Done`].
pub struct Editor {
    /// Prompt printed before the text on every redraw.
    prompt: String,
    /// Maximum number of characters (not bytes) allowed in the line.
    max_chars: usize,
    /// In secret mode every character echoes as `*` and history is disabled.
    secret: bool,
    /// Raw buffer bytes of the line being edited.
    buf: String,
    /// Byte offset of the cursor; always on a char boundary.
    cursor: usize,
    /// Current parser state.
    state: ParseState,
    /// Number of bytes consumed by the current escape sequence.
    esc_len: usize,
    /// Parameter bytes collected inside a CSI sequence.
    params: Vec<u8>,
    /// Bytes of an incomplete UTF-8 sequence.
    utf8: Vec<u8>,
    /// Expected total length of the UTF-8 sequence being buffered.
    utf8_target: usize,
    /// History, oldest first, at most [`MAX_HISTORY`] entries.
    history: Vec<String>,
    /// How many history entries the user has moved up; 0 means editing.
    hist_offset: usize,
    /// Saved live line while browsing history.
    stash: String,
}

impl Editor {
    /// Create an editor that displays `prompt` before the text and allows at
    /// most `max_chars` characters (counted in characters, not bytes).
    #[must_use]
    pub fn new(prompt: &str, max_chars: usize) -> Editor {
        Self::build(prompt, max_chars, false)
    }

    /// Create an editor in secret mode: every character is echoed as `*` and
    /// history is never recorded or recalled. [`Action::Done`] still reports
    /// the real text.
    #[must_use]
    pub fn secret(prompt: &str, max_chars: usize) -> Editor {
        Self::build(prompt, max_chars, true)
    }

    /// Shared constructor for normal and secret mode.
    fn build(prompt: &str, max_chars: usize, secret: bool) -> Editor {
        Editor {
            prompt: prompt.to_string(),
            max_chars,
            secret,
            buf: String::new(),
            cursor: 0,
            state: ParseState::Normal,
            esc_len: 0,
            params: Vec::new(),
            utf8: Vec::new(),
            utf8_target: 0,
            history: Vec::new(),
            hist_offset: 0,
            stash: String::new(),
        }
    }

    /// Remember `line` for Up/Down history navigation.
    ///
    /// Empty lines and consecutive duplicates are ignored; only the most
    /// recent 50 lines are kept. This is a no-op in secret mode.
    pub fn remember(&mut self, line: &str) {
        if self.secret || line.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|last| last == line) {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
    }

    /// Feed one byte and return what the caller should do with it.
    ///
    /// Invalid bytes are always swallowed safely; this function never panics,
    /// whatever byte sequence is fed to it.
    pub fn feed(&mut self, byte: u8) -> Action {
        if !self.utf8.is_empty() {
            return self.utf8_byte(byte);
        }
        if self.state == ParseState::Normal {
            self.normal_byte(byte)
        } else {
            self.escape_byte(byte)
        }
    }

    /// Handle a byte while a UTF-8 sequence is incomplete.
    fn utf8_byte(&mut self, byte: u8) -> Action {
        if !(0x80..=0xbf).contains(&byte) {
            // Not a continuation byte: discard the partial sequence, drop the
            // offending byte and report a bell.
            self.utf8.clear();
            self.utf8_target = 0;
            return Action::Bell;
        }
        self.utf8.push(byte);
        if self.utf8.len() < self.utf8_target {
            return Action::None;
        }
        let seq = std::mem::take(&mut self.utf8);
        self.utf8_target = 0;
        match std::str::from_utf8(&seq) {
            // Rust's validator rejects overlong sequences and surrogates.
            Ok(text) => match text.chars().next() {
                Some(ch) => self.insert_char(ch),
                None => Action::Bell,
            },
            Err(_) => Action::Bell,
        }
    }

    /// Handle a byte in ordinary (non-escape) state.
    fn normal_byte(&mut self, byte: u8) -> Action {
        match byte {
            0x20..=0x7e => self.insert_char(char::from(byte)),
            0x0d | 0x0a => {
                let text = std::mem::take(&mut self.buf);
                self.cursor = 0;
                self.reset_edit();
                Action::Done(text)
            }
            0x03 => {
                self.clear_line();
                Action::Cancelled
            }
            0x04 => {
                if self.buf.is_empty() {
                    self.clear_line();
                    Action::EndOfInput
                } else {
                    Action::None
                }
            }
            0x08 | 0x7f => self.backspace(),
            0x15 => self.kill_before_cursor(),
            0x17 => self.delete_word_back(),
            0x01 => self.home(),
            0x05 => self.end(),
            0x1b => {
                self.state = ParseState::Esc;
                self.esc_len = 1;
                self.params.clear();
                Action::None
            }
            0x80..=0xbf => Action::Bell,
            0xc0..=0xdf => self.start_utf8(byte, 2),
            0xe0..=0xef => self.start_utf8(byte, 3),
            0xf0..=0xf4 => self.start_utf8(byte, 4),
            0xf5..=0xff => Action::Bell,
            _ => Action::None,
        }
    }

    /// Begin buffering a UTF-8 sequence starting with `lead`.
    fn start_utf8(&mut self, lead: u8, len: usize) -> Action {
        self.utf8.clear();
        self.utf8.push(lead);
        self.utf8_target = len;
        Action::None
    }

    /// Handle a byte while inside an escape sequence.
    fn escape_byte(&mut self, byte: u8) -> Action {
        self.esc_len += 1;
        if self.esc_len > MAX_ESC_LEN {
            self.reset_escape();
            return Action::None;
        }
        match self.state {
            ParseState::Esc => {
                if byte == b'[' {
                    self.state = ParseState::Csi;
                } else if byte == b'O' {
                    self.state = ParseState::Ss3;
                } else if byte == 0x1b {
                    // Esc Esc: restart and swallow the first one.
                    self.esc_len = 1;
                } else {
                    // A lone ESC followed by a letter is swallowed too.
                    self.reset_escape();
                }
                Action::None
            }
            ParseState::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    let action = self.dispatch_csi(byte);
                    self.reset_escape();
                    action
                } else {
                    if self.params.len() < MAX_PARAMS {
                        self.params.push(byte);
                    }
                    Action::None
                }
            }
            ParseState::Ss3 => {
                if (0x40..=0x7e).contains(&byte) {
                    let action = match byte {
                        b'H' => self.home(),
                        b'F' => self.end(),
                        b'A' => self.up(),
                        b'B' => self.down(),
                        b'C' => self.right(),
                        b'D' => self.left(),
                        _ => Action::None,
                    };
                    self.reset_escape();
                    action
                } else {
                    Action::None
                }
            }
            ParseState::Normal => Action::None,
        }
    }

    /// Dispatch a complete CSI sequence whose final byte is `final_byte`.
    fn dispatch_csi(&mut self, final_byte: u8) -> Action {
        match final_byte {
            b'H' => self.home(),
            b'F' => self.end(),
            b'A' => self.up(),
            b'B' => self.down(),
            b'C' => self.right(),
            b'D' => self.left(),
            b'~' => {
                let params = self.params.as_slice();
                if params == b"1" {
                    self.home()
                } else if params == b"3" {
                    self.delete_at_cursor()
                } else if params == b"4" {
                    self.end()
                } else {
                    Action::None
                }
            }
            _ => Action::None,
        }
    }

    /// Leave escape-sequence state and clear collected parameters.
    fn reset_escape(&mut self) {
        self.state = ParseState::Normal;
        self.esc_len = 0;
        self.params.clear();
    }

    /// Number of characters (not bytes) currently in the buffer.
    fn char_count(&self) -> usize {
        self.buf.chars().count()
    }

    /// Number of characters after the cursor.
    fn chars_after_cursor(&self) -> usize {
        self.buf
            .char_indices()
            .filter(|&(i, _)| i >= self.cursor)
            .count()
    }

    /// Insert `ch` at the cursor, respecting the character limit.
    fn insert_char(&mut self, ch: char) -> Action {
        if self.char_count() >= self.max_chars {
            return Action::Bell;
        }
        self.buf.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.reset_edit();
        Action::Redraw(self.render())
    }

    /// Delete the character before the cursor, if any.
    fn backspace(&mut self) -> Action {
        let prev = self
            .buf
            .char_indices()
            .rfind(|&(i, _)| i < self.cursor)
            .map(|(i, ch)| (i, ch.len_utf8()));
        match prev {
            Some((start, len)) => {
                self.buf.replace_range(start..start + len, "");
                self.cursor = start;
                self.reset_edit();
                Action::Redraw(self.render())
            }
            None => Action::Bell,
        }
    }

    /// Delete the character at the cursor, if any.
    fn delete_at_cursor(&mut self) -> Action {
        let next = self
            .buf
            .char_indices()
            .find(|&(i, _)| i >= self.cursor)
            .map(|(i, ch)| (i, ch.len_utf8()));
        match next {
            Some((start, len)) => {
                self.buf.replace_range(start..start + len, "");
                self.reset_edit();
                Action::Redraw(self.render())
            }
            None => Action::Bell,
        }
    }

    /// Delete everything before the cursor (Ctrl-U).
    fn kill_before_cursor(&mut self) -> Action {
        if self.cursor == 0 {
            return Action::Bell;
        }
        self.buf.replace_range(0..self.cursor, "");
        self.cursor = 0;
        self.reset_edit();
        Action::Redraw(self.render())
    }

    /// Delete the word before the cursor (Ctrl-W).
    fn delete_word_back(&mut self) -> Action {
        if self.cursor == 0 {
            return Action::Bell;
        }
        let mut start = self.cursor;
        let mut in_word = false;
        for (i, ch) in self.buf.char_indices().rev() {
            if i >= self.cursor {
                continue;
            }
            if in_word {
                if ch.is_whitespace() {
                    break;
                }
                start = i;
            } else if ch.is_whitespace() {
                start = i;
            } else {
                in_word = true;
                start = i;
            }
        }
        self.buf.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.reset_edit();
        Action::Redraw(self.render())
    }

    /// Move the cursor to the beginning of the line.
    fn home(&mut self) -> Action {
        if self.cursor == 0 {
            Action::None
        } else {
            self.cursor = 0;
            Action::Redraw(self.render())
        }
    }

    /// Move the cursor to the end of the line.
    fn end(&mut self) -> Action {
        if self.cursor == self.buf.len() {
            Action::None
        } else {
            self.cursor = self.buf.len();
            Action::Redraw(self.render())
        }
    }

    /// Move the cursor one character to the left.
    fn left(&mut self) -> Action {
        let target = self
            .buf
            .char_indices()
            .rfind(|&(i, _)| i < self.cursor)
            .map(|(i, _)| i);
        match target {
            Some(i) => {
                self.cursor = i;
                Action::Redraw(self.render())
            }
            None => Action::None,
        }
    }

    /// Move the cursor one character to the right.
    fn right(&mut self) -> Action {
        let target = self
            .buf
            .char_indices()
            .find(|&(i, _)| i >= self.cursor)
            .map(|(i, ch)| i + ch.len_utf8());
        match target {
            Some(i) => {
                self.cursor = i;
                Action::Redraw(self.render())
            }
            None => Action::None,
        }
    }

    /// Move one entry up in the history (Up arrow).
    fn up(&mut self) -> Action {
        if self.secret {
            return Action::None;
        }
        if self.hist_offset >= self.history.len() {
            return Action::Bell;
        }
        if self.hist_offset == 0 {
            self.stash = self.buf.clone();
        }
        self.hist_offset += 1;
        self.load_history();
        Action::Redraw(self.render())
    }

    /// Move one entry down in the history; past the newest entry the
    /// line being typed is restored (Down arrow).
    fn down(&mut self) -> Action {
        if self.secret || self.hist_offset == 0 {
            return Action::None;
        }
        self.hist_offset -= 1;
        if self.hist_offset == 0 {
            self.buf = std::mem::take(&mut self.stash);
            self.cursor = self.buf.len();
        } else {
            self.load_history();
        }
        Action::Redraw(self.render())
    }

    /// Load the history entry `hist_offset` positions from the newest into
    /// the buffer, placing the cursor at the end.
    fn load_history(&mut self) {
        let idx = self.history.len().saturating_sub(self.hist_offset);
        if let Some(entry) = self.history.get(idx) {
            self.buf.clone_from(entry);
            self.cursor = self.buf.len();
        }
    }

    /// Leave history-browsing mode; called after any edit.
    fn reset_edit(&mut self) {
        self.hist_offset = 0;
        self.stash.clear();
    }

    /// Discard the buffer and reset all per-line state.
    fn clear_line(&mut self) {
        self.buf.clear();
        self.cursor = 0;
        self.reset_edit();
    }

    /// Render the `\r prompt text \x1b[K [\x1b[nD]` byte sequence.
    fn render(&self) -> String {
        let after = self.chars_after_cursor();
        let mut out = String::with_capacity(self.prompt.len() + self.buf.len() * 2 + 16);
        out.push('\r');
        out.push_str(&self.prompt);
        if self.secret {
            out.push_str(&"*".repeat(self.char_count()));
        } else {
            out.push_str(&self.buf);
        }
        out.push_str("\x1b[K");
        if after > 0 {
            out.push_str("\x1b[");
            out.push_str(after.to_string().as_str());
            out.push('D');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole byte string, returning the last action.
    fn feed_all(editor: &mut Editor, bytes: &[u8]) -> Action {
        let mut last = Action::None;
        for &byte in bytes {
            last = editor.feed(byte);
        }
        last
    }

    /// Type a whole string.
    fn type_str(editor: &mut Editor, text: &str) {
        for byte in text.bytes() {
            editor.feed(byte);
        }
    }

    #[test]
    fn plain_typing_and_done() {
        let mut e = Editor::new("> ", 64);
        assert_eq!(e.feed(b'a'), Action::Redraw("\r> a\x1b[K".to_string()));
        type_str(&mut e, "bc");
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("abc".to_string()));
        // Editor is reusable after Done.
        type_str(&mut e, "x");
        assert_eq!(feed_all(&mut e, &[0x0a]), Action::Done("x".to_string()));
    }

    #[test]
    fn unicode_typing_and_editing() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "héllo wörld");
        assert_eq!(
            feed_all(&mut e, &[0x0d]),
            Action::Done("héllo wörld".to_string())
        );
    }

    #[test]
    fn utf8_multibyte_echo_exact() {
        let mut e = Editor::new("$ ", 10);
        assert_eq!(e.feed(0xc3), Action::None);
        assert_eq!(e.feed(0xa9), Action::Redraw("\r$ é\x1b[K".to_string()));
    }

    #[test]
    fn cursor_movement_then_insert() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "hello");
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'D']),
            Action::Redraw("\r> hello\x1b[K\x1b[1D".to_string())
        );
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'D']),
            Action::Redraw("\r> hello\x1b[K\x1b[2D".to_string())
        );
        type_str(&mut e, "XY");
        assert_eq!(
            feed_all(&mut e, &[0x0d]),
            Action::Done("helXYlo".to_string())
        );
    }

    #[test]
    fn left_right_arrows_and_boundaries() {
        let mut e = Editor::new("> ", 64);
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'D']), Action::None);
        type_str(&mut e, "ab");
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'D']),
            Action::Redraw("\r> ab\x1b[K\x1b[1D".to_string())
        );
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'C']),
            Action::Redraw("\r> ab\x1b[K".to_string())
        );
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'C']), Action::None);
    }

    #[test]
    fn left_over_multibyte_char_moves_whole_char() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "é");
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'D']),
            Action::Redraw("\r> é\x1b[K\x1b[1D".to_string())
        );
        // Left again at start of line: no move.
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'D']), Action::None);
    }

    #[test]
    fn backspace_deletes_whole_utf8_char() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "aéb");
        assert_eq!(e.feed(0x7f), Action::Redraw("\r> aé\x1b[K".to_string()));
        assert_eq!(e.feed(0x7f), Action::Redraw("\r> a\x1b[K".to_string()));
        assert_eq!(e.feed(0x08), Action::Redraw("\r> \x1b[K".to_string()));
        assert_eq!(e.feed(0x7f), Action::Bell);
    }

    #[test]
    fn delete_key_removes_at_cursor() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "abc");
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'D']),
            Action::Redraw("\r> abc\x1b[K\x1b[1D".to_string())
        );
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'3', b'~']),
            Action::Redraw("\r> ab\x1b[K".to_string())
        );
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'3', b'~']), Action::Bell);
    }

    #[test]
    fn ctrl_u_kills_before_cursor() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "abc");
        assert_eq!(
            e.feed(0x01),
            Action::Redraw("\r> abc\x1b[K\x1b[3D".to_string())
        );
        assert_eq!(e.feed(0x05), Action::Redraw("\r> abc\x1b[K".to_string()));
        // Move to middle: home, right once.
        assert_eq!(
            e.feed(0x01),
            Action::Redraw("\r> abc\x1b[K\x1b[3D".to_string())
        );
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'C']),
            Action::Redraw("\r> abc\x1b[K\x1b[2D".to_string())
        );
        // The cursor is now at the start of "bc".
        assert_eq!(
            e.feed(0x15),
            Action::Redraw("\r> bc\x1b[K\x1b[2D".to_string())
        );
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("bc".to_string()));

        // Ctrl-U at start of line bells.
        let mut e2 = Editor::new("> ", 64);
        assert_eq!(e2.feed(0x15), Action::Bell);
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "foo bar");
        assert_eq!(e.feed(0x17), Action::Redraw("\r> foo \x1b[K".to_string()));
        assert_eq!(e.feed(0x17), Action::Redraw("\r> \x1b[K".to_string()));
        assert_eq!(e.feed(0x17), Action::Bell);

        // Multiple spaces and mid-word cursor.
        let mut e2 = Editor::new("> ", 64);
        type_str(&mut e2, "foo  bar");
        assert_eq!(
            feed_all(
                &mut e2,
                &[0x1b, b'[', b'D', 0x1b, b'[', b'D', 0x1b, b'[', b'D']
            ),
            Action::Redraw("\r> foo  bar\x1b[K\x1b[3D".to_string())
        );
        assert_eq!(
            e2.feed(0x17),
            Action::Redraw("\r> bar\x1b[K\x1b[3D".to_string())
        );
    }

    #[test]
    fn ctrl_w_deletes_word_before_cursor() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "one two three");
        assert_eq!(
            e.feed(0x17),
            Action::Redraw("\r> one two \x1b[K".to_string())
        );
        assert_eq!(
            feed_all(&mut e, &[0x0d]),
            Action::Done("one two ".to_string())
        );
    }

    #[test]
    fn history_navigation_including_restore() {
        let mut e = Editor::new("$ ", 64);
        e.remember("first");
        e.remember("second");
        e.remember("second"); // consecutive duplicate ignored
        e.remember(""); // empty ignored
        type_str(&mut e, "draft");
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'A']),
            Action::Redraw("\r$ second\x1b[K".to_string())
        );
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'A']),
            Action::Redraw("\r$ first\x1b[K".to_string())
        );
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'A']), Action::Bell);
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'B']),
            Action::Redraw("\r$ second\x1b[K".to_string())
        );
        // Past the newest entry the line being typed is restored.
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'B']),
            Action::Redraw("\r$ draft\x1b[K".to_string())
        );
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'B']), Action::None);
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("draft".to_string()));
    }

    #[test]
    fn editing_while_browsing_history_resets_navigation() {
        let mut e = Editor::new("$ ", 64);
        e.remember("stored");
        type_str(&mut e, "x");
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'A']),
            Action::Redraw("\r$ stored\x1b[K".to_string())
        );
        type_str(&mut e, "y");
        assert_eq!(
            feed_all(&mut e, &[0x0d]),
            Action::Done("storedy".to_string())
        );
    }

    #[test]
    fn history_keeps_newest_50() {
        let mut e = Editor::new("$ ", 128);
        for i in 0..60 {
            e.remember(format!("line{i}").as_str());
        }
        for _ in 0..49 {
            feed_all(&mut e, &[0x1b, b'[', b'A']);
        }
        assert_eq!(
            feed_all(&mut e, &[0x1b, b'[', b'A']),
            Action::Redraw("\r$ line10\x1b[K".to_string())
        );
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'A']), Action::Bell);
        assert_eq!(
            feed_all(&mut e, &[0x0d]),
            Action::Done("line10".to_string())
        );
    }

    #[test]
    fn truncated_escape_sequence_followed_by_text() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "ab");
        assert_eq!(e.feed(0x1b), Action::None);
        assert_eq!(e.feed(b'['), Action::None);
        assert_eq!(e.feed(b'3'), Action::None);
        assert_eq!(e.feed(b'x'), Action::None); // unknown final byte swallowed
        type_str(&mut e, "cd");
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("abcd".to_string()));
    }

    #[test]
    fn ss3_unknown_final_byte_is_swallowed() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "ab");
        assert_eq!(e.feed(0x1b), Action::None);
        assert_eq!(e.feed(b'O'), Action::None);
        assert_eq!(e.feed(b'q'), Action::None);
        type_str(&mut e, "cd");
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("abcd".to_string()));
    }

    #[test]
    fn lone_escape_swallows_letter() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "ab");
        assert_eq!(e.feed(0x1b), Action::None);
        assert_eq!(e.feed(b'a'), Action::None); // swallowed, not inserted
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("ab".to_string()));
    }

    #[test]
    fn home_and_end_variants() {
        let homes: &[&[u8]] = &[b"\x1b[H", b"\x1b[1~", b"\x1bOH"];
        for &seq in homes {
            let mut e = Editor::new("> ", 64);
            type_str(&mut e, "abc");
            assert_eq!(
                feed_all(&mut e, seq),
                Action::Redraw("\r> abc\x1b[K\x1b[3D".to_string())
            );
            assert_eq!(
                feed_all(&mut e, &[0x05]),
                Action::Redraw("\r> abc\x1b[K".to_string())
            );
        }
        let ends: &[&[u8]] = &[b"\x1b[F", b"\x1b[4~", b"\x1bOF"];
        for &seq in ends {
            let mut e = Editor::new("> ", 64);
            type_str(&mut e, "abc");
            assert_eq!(
                feed_all(&mut e, &[0x01]),
                Action::Redraw("\r> abc\x1b[K\x1b[3D".to_string())
            );
            assert_eq!(
                feed_all(&mut e, seq),
                Action::Redraw("\r> abc\x1b[K".to_string())
            );
        }
    }

    #[test]
    fn invalid_utf8_is_discarded() {
        let mut e = Editor::new("> ", 64);
        assert_eq!(e.feed(0xff), Action::Bell);
        assert_eq!(e.feed(0x80), Action::Bell);
        // Overlong encoding of '/'.
        assert_eq!(e.feed(0xc0), Action::None);
        assert_eq!(e.feed(0xaf), Action::Bell);
        // Truncated 2-byte sequence interrupted by a non-continuation byte.
        assert_eq!(e.feed(0xc3), Action::None);
        assert_eq!(e.feed(0x28), Action::Bell);
        // A well-formed multi-byte character still works afterwards.
        assert_eq!(e.feed(0xc3), Action::None);
        assert_eq!(e.feed(0xa9), Action::Redraw("\r> é\x1b[K".to_string()));
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("é".to_string()));
    }

    #[test]
    fn max_chars_boundary() {
        let mut e = Editor::new("> ", 2);
        type_str(&mut e, "ab");
        assert_eq!(e.feed(b'c'), Action::Bell);
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("ab".to_string()));

        // Multi-byte characters count as one character each.
        let mut e2 = Editor::new("> ", 1);
        assert_eq!(e2.feed(0xc3), Action::None);
        assert_eq!(e2.feed(0xa9), Action::Redraw("\r> é\x1b[K".to_string()));
        assert_eq!(e2.feed(b'x'), Action::Bell);

        // A zero limit rejects everything but still accepts Enter.
        let mut e3 = Editor::new("> ", 0);
        assert_eq!(e3.feed(b'x'), Action::Bell);
        assert_eq!(feed_all(&mut e3, &[0x0d]), Action::Done(String::new()));
    }

    #[test]
    fn secret_mode_echoes_stars_and_never_stores_history() {
        let mut e = Editor::secret("pw: ", 16);
        e.remember("topsecret"); // must be ignored
        assert_eq!(e.feed(b'p'), Action::Redraw("\rpw: *\x1b[K".to_string()));
        type_str(&mut e, "w");
        assert_eq!(e.feed(b'x'), Action::Redraw("\rpw: ***\x1b[K".to_string()));
        // No history available: Up is a no-op.
        assert_eq!(feed_all(&mut e, &[0x1b, b'[', b'A']), Action::None);
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("pwx".to_string()));
    }

    #[test]
    fn ctrl_c_cancels_and_ctrl_d_signals_end_of_input() {
        let mut e = Editor::new("> ", 64);
        assert_eq!(e.feed(0x03), Action::Cancelled);

        let mut e2 = Editor::new("> ", 64);
        assert_eq!(e2.feed(0x04), Action::EndOfInput);

        // Ctrl-D with text present is ignored.
        let mut e3 = Editor::new("> ", 64);
        type_str(&mut e3, "text");
        assert_eq!(e3.feed(0x04), Action::None);
        assert_eq!(feed_all(&mut e3, &[0x0a]), Action::Done("text".to_string()));
    }

    #[test]
    fn cr_and_lf_both_finish_the_line() {
        for byte in [0x0d, 0x0a] {
            let mut e = Editor::new("> ", 64);
            type_str(&mut e, "q");
            assert_eq!(e.feed(byte), Action::Done("q".to_string()));
        }
    }

    #[test]
    fn done_clears_line_for_reuse() {
        let mut e = Editor::new("> ", 64);
        type_str(&mut e, "one");
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("one".to_string()));
        type_str(&mut e, "two");
        assert_eq!(feed_all(&mut e, &[0x0d]), Action::Done("two".to_string()));
    }

    #[test]
    fn fuzz_all_bytes_in_several_orders() {
        let mut editor = Editor::new("> ", 16);
        for offset in [0u16, 1, 63, 128, 255] {
            for i in 0..256u16 {
                let _ = editor.feed(u8::try_from(i.wrapping_add(offset) & 0xff).unwrap_or(0));
            }
        }
        let _ = editor.feed(0x0d);

        let mut editor = Editor::new("> ", 16);
        for i in (0..=255u8).rev() {
            let _ = editor.feed(i);
        }
        let _ = editor.feed(0x0d);

        let mut editor = Editor::new("> ", 16);
        let mut seed: u32 = 0x1234_5678;
        for _ in 0..4096 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let _ = editor.feed(u8::try_from(seed >> 24).unwrap_or(0));
        }
        let _ = editor.feed(0x0d);
    }
}
