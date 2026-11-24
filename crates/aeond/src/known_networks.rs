//! Remembered Wi-Fi networks held entirely in memory.
//!
//! The caller owns all file or network I/O. This module validates entries,
//! parses and renders a tab-separated text format, and redacts secrets from
//! diagnostics. SSIDs are arbitrary bytes on the wire, so they are represented
//! as raw bytes here; passphrases are printable ASCII only.

use std::fmt;

const MAX_ENTRIES: usize = 64;
const MAX_SSID_BYTES: usize = 32;
const MIN_PASSPHRASE_BYTES: usize = 8;
const MAX_PASSPHRASE_BYTES: usize = 63;

const ERR_WRONG_FIELD_COUNT: &str = "wrong field count";
const ERR_BAD_HEX: &str = "bad hex";
const ERR_SSID_LENGTH: &str = "SSID must be 1 to 32 bytes";
const ERR_PASSPHRASE_LENGTH: &str = "passphrase must be 8 to 63 printable ASCII characters";
const ERR_PASSPHRASE_PRINTABLE: &str = "passphrase must be printable ASCII";
const ERR_LAST_USED: &str = "last_used must be an unsigned 64-bit decimal integer";

/// A remembered network.
///
/// `ssid` is 1..=32 arbitrary bytes. `passphrase` is `None` for an open
/// network; when present it is 8..=63 printable ASCII characters. `last_used`
/// is an opaque counter where larger means more recently used.
pub struct Entry {
    /// Network SSID as raw bytes, 1..=32 bytes long.
    pub ssid: Vec<u8>,
    /// Passphrase, or `None` for an open network.
    pub passphrase: Option<Secret>,
    /// Opaque last-used counter; larger means more recently used.
    pub last_used: u64,
}

impl fmt::Debug for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Entry")
            .field("ssid_hex", &encode_hex(&self.ssid))
            .field("passphrase", &self.passphrase)
            .field("last_used", &self.last_used)
            .finish()
    }
}

/// A validated passphrase that never exposes itself through `Debug`.
///
/// The inner string is overwritten with zero bytes when dropped. There is no
/// `Display` implementation; callers must opt in to exposure explicitly.
pub struct Secret(String);

impl Secret {
    /// Creates a secret from `text` after validating printable-ASCII bounds.
    ///
    /// # Errors
    /// [`BookError::InvalidPassphrase`] when the length or characters are out of range.
    pub fn new(text: String) -> Result<Secret, BookError> {
        let bytes = text.into_bytes();
        validate_passphrase_bytes(&bytes).map_err(|_| BookError::InvalidPassphrase)?;
        let text = String::from_utf8(bytes).map_err(|_| BookError::InvalidPassphrase)?;
        Ok(Secret(text))
    }

    /// Borrows the passphrase for explicit use by the caller.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(***)")
    }
}

/// Errors produced by validating an entry or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookError {
    /// The SSID was empty or longer than 32 bytes.
    InvalidSsid,
    /// The passphrase was outside 8..=63 printable ASCII bytes.
    InvalidPassphrase,
}

impl fmt::Display for BookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BookError::InvalidSsid => formatter.write_str("SSID must be 1 to 32 bytes"),
            BookError::InvalidPassphrase => {
                formatter.write_str("passphrase must be 8 to 63 printable ASCII characters")
            }
        }
    }
}

impl std::error::Error for BookError {}

/// A parse failure on a single input line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    /// One-based line number in the parsed text.
    pub line: usize,
    /// Static description of why the line was skipped.
    pub message: &'static str,
}

impl fmt::Display for LineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for LineError {}

/// A bounded set of remembered networks.
pub struct Book {
    entries: Vec<Entry>,
}

impl Book {
    /// Creates an empty book.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Returns the number of stored entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no entries are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns all entries in storage order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Validates and inserts or replaces an entry by SSID.
    ///
    /// If the book is already at capacity after insertion, the entry with the
    /// smallest `last_used` is evicted.
    ///
    /// # Errors
    /// [`BookError`] when the SSID or passphrase is invalid.
    pub fn upsert(&mut self, entry: Entry) -> Result<(), BookError> {
        validate_entry(&entry)?;
        self.insert_entry(entry);
        Ok(())
    }

    /// Removes the entry with `ssid`, returning true if one was present.
    pub fn forget(&mut self, ssid: &[u8]) -> bool {
        if let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.ssid.as_slice() == ssid)
        {
            let _ = self.entries.remove(position);
            true
        } else {
            false
        }
    }

    /// Returns the best known network visible right now.
    ///
    /// The visible list contains `(ssid, signal_dbm)` pairs. Stronger signal
    /// wins; equal signal is broken by the remembered entry's higher
    /// `last_used`. Unknown SSIDs are ignored.
    #[must_use]
    pub fn best_match(&self, visible: &[(Vec<u8>, i32)]) -> Option<&Entry> {
        let mut best: Option<&Entry> = None;
        let mut best_signal = 0_i32;

        for visible_network in visible {
            let (ssid, signal) = visible_network;
            let Some(entry) = self
                .entries
                .iter()
                .find(|candidate| candidate.ssid.as_slice() == ssid.as_slice())
            else {
                continue;
            };

            let replace = match best {
                None => true,
                Some(current) => {
                    *signal > best_signal
                        || (*signal == best_signal && entry.last_used > current.last_used)
                }
            };

            if replace {
                best = Some(entry);
                best_signal = *signal;
            }
        }

        best
    }

    /// Parses remembered networks, one per line.
    ///
    /// Each line is `hex(ssid) TAB hex(passphrase) TAB last_used`, with `-` as
    /// the passphrase field for an open network. Hex is lowercase on output and
    /// either case is accepted on input. Malformed lines are skipped and
    /// reported with one-based line numbers. A later valid line for the same
    /// SSID replaces the earlier one.
    #[must_use]
    pub fn parse(text: &str) -> (Book, Vec<LineError>) {
        let mut book = Book::new();
        let mut errors = Vec::new();

        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            match entry_from_line(line) {
                Ok(entry) => book.insert_entry(entry),
                Err(message) => errors.push(LineError {
                    line: line_number,
                    message,
                }),
            }
        }

        (book, errors)
    }

    /// Renders the book, one network per line, sorted by `last_used` descending.
    ///
    /// Output ends with a trailing newline when at least one entry is present.
    #[must_use]
    pub fn render(&self) -> String {
        let mut entries: Vec<&Entry> = self.entries.iter().collect();
        entries.sort_by(|left, right| {
            right
                .last_used
                .cmp(&left.last_used)
                .then_with(|| left.ssid.cmp(&right.ssid))
        });

        let mut output = String::new();
        for entry in entries {
            output.push_str(&encode_hex(&entry.ssid));
            output.push('\t');
            match &entry.passphrase {
                Some(secret) => output.push_str(&encode_hex(secret.expose().as_bytes())),
                None => output.push('-'),
            }
            output.push('\t');
            output.push_str(&entry.last_used.to_string());
            output.push('\n');
        }
        output
    }

    fn insert_entry(&mut self, entry: Entry) {
        if let Some(position) = self
            .entries
            .iter()
            .position(|existing| existing.ssid == entry.ssid)
        {
            if let Some(slot) = self.entries.get_mut(position) {
                *slot = entry;
            }
            return;
        }

        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let lowest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, candidate)| candidate.last_used)
                .map(|(position, _)| position);
            if let Some(position) = lowest {
                let _ = self.entries.remove(position);
            }
        }
    }
}

impl Default for Book {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Book {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Book")
            .field("len", &self.entries.len())
            .finish()
    }
}

fn validate_entry(entry: &Entry) -> Result<(), BookError> {
    if entry.ssid.is_empty() || entry.ssid.len() > MAX_SSID_BYTES {
        return Err(BookError::InvalidSsid);
    }
    if let Some(secret) = &entry.passphrase {
        validate_passphrase_bytes(secret.expose().as_bytes())
            .map_err(|_| BookError::InvalidPassphrase)?;
    }
    Ok(())
}

fn validate_passphrase_bytes(bytes: &[u8]) -> Result<(), &'static str> {
    if !(MIN_PASSPHRASE_BYTES..=MAX_PASSPHRASE_BYTES).contains(&bytes.len()) {
        return Err(ERR_PASSPHRASE_LENGTH);
    }
    if bytes.iter().any(|byte| !(0x20_u8..=0x7e_u8).contains(byte)) {
        return Err(ERR_PASSPHRASE_PRINTABLE);
    }
    Ok(())
}

fn entry_from_line(line: &str) -> Result<Entry, &'static str> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [ssid_field, passphrase_field, last_used_field] = fields.as_slice() else {
        return Err(ERR_WRONG_FIELD_COUNT);
    };

    let ssid = decode_hex(ssid_field)?;
    if ssid.is_empty() || ssid.len() > MAX_SSID_BYTES {
        return Err(ERR_SSID_LENGTH);
    }

    let passphrase = if *passphrase_field == "-" {
        None
    } else {
        let bytes = decode_hex(passphrase_field)?;
        let text = secret_text_from_bytes(bytes)?;
        Some(Secret(text))
    };

    let last_used = parse_u64(last_used_field).ok_or(ERR_LAST_USED)?;
    Ok(Entry {
        ssid,
        passphrase,
        last_used,
    })
}

fn secret_text_from_bytes(bytes: Vec<u8>) -> Result<String, &'static str> {
    validate_passphrase_bytes(&bytes)?;
    String::from_utf8(bytes).map_err(|_| ERR_PASSPHRASE_PRINTABLE)
}

fn decode_hex(text: &str) -> Result<Vec<u8>, &'static str> {
    let (chunks, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(ERR_BAD_HEX);
    }

    let mut bytes = Vec::with_capacity(text.len() / 2);
    for [high, low] in chunks {
        let high = hex_value(*high).ok_or(ERR_BAD_HEX)?;
        let low = hex_value(*low).ok_or(ERR_BAD_HEX)?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    fn hex_char(nibble: u8) -> char {
        match nibble {
            0..=9 => char::from(b'0' + nibble),
            _ => char::from(b'a' + nibble - 10),
        }
    }

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_char(byte >> 4));
        output.push(hex_char(byte & 0x0f));
    }
    output
}

fn parse_u64(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_entry(ssid: &[u8], last_used: u64) -> Entry {
        Entry {
            ssid: ssid.to_vec(),
            passphrase: None,
            last_used,
        }
    }

    fn secured_entry(ssid: &[u8], passphrase: &str, last_used: u64) -> Entry {
        Entry {
            ssid: ssid.to_vec(),
            passphrase: Some(Secret::new(passphrase.to_owned()).unwrap()),
            last_used,
        }
    }

    fn one_byte_entry(ssid: u8, passphrase: Option<&str>, last_used: u64) -> Entry {
        Entry {
            ssid: vec![ssid],
            passphrase: passphrase.map(|text| Secret::new(text.to_owned()).unwrap()),
            last_used,
        }
    }

    fn assert_parse_error(text: &str, line: usize, message: &'static str) {
        let (book, errors) = Book::parse(text);
        assert_eq!(book.len(), 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, line);
        assert_eq!(errors[0].message, message);
    }

    fn assert_second_line_error(text: &str, message: &'static str) {
        let (book, errors) = Book::parse(text);
        assert_eq!(book.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 2);
        assert_eq!(errors[0].message, message);
    }

    #[test]
    fn round_trip_preserves_entries() {
        let mut book = Book::new();
        book.upsert(secured_entry(&[0xde, 0xad, 0xbe, 0xef], "p@ss w0rd", 7))
            .unwrap();
        book.upsert(open_entry(b"open", 9)).unwrap();

        let text = book.render();
        let (parsed, errors) = Book::parse(&text);

        assert!(errors.is_empty());
        assert_eq!(parsed.render(), text);
        assert_eq!(parsed.entries().len(), 2);
    }

    #[test]
    fn render_sorts_by_last_used_descending_and_ends_with_newline() {
        let mut book = Book::new();
        book.upsert(open_entry(b"old", 1)).unwrap();
        book.upsert(open_entry(b"new", 3)).unwrap();
        book.upsert(open_entry(b"mid", 2)).unwrap();

        let text = book.render();
        assert!(text.ends_with('\n'));
        let fields: Vec<&str> = text.lines().collect();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].rsplit('\t').next(), Some("3"));
        assert_eq!(fields[1].rsplit('\t').next(), Some("2"));
        assert_eq!(fields[2].rsplit('\t').next(), Some("1"));
    }

    #[test]
    fn render_of_empty_book_is_empty() {
        assert_eq!(Book::new().render(), "");
    }

    #[test]
    fn parse_accepts_uppercase_hex_and_open_networks() {
        let (book, errors) =
            Book::parse("AABB\t-\t18446744073709551615\n00\t2D2D2D2D2D2D2D2D\t1\n");
        assert!(errors.is_empty());
        assert_eq!(book.len(), 2);
        assert_eq!(book.entries()[0].ssid, vec![0xaa, 0xbb]);
        assert_eq!(book.entries()[0].last_used, u64::MAX);
    }

    #[test]
    fn parse_wrong_field_count_reports_line() {
        assert_parse_error("\n", 1, ERR_WRONG_FIELD_COUNT);
        assert_parse_error("00\t-\t1\textra\n", 1, ERR_WRONG_FIELD_COUNT);
        assert_parse_error("00\t1\n", 1, ERR_WRONG_FIELD_COUNT);
        assert_second_line_error("aa\t-\t1\nextra\t-\t2\textra\n", ERR_WRONG_FIELD_COUNT);
    }

    #[test]
    fn parse_reports_bad_hex_and_line_numbers() {
        assert_parse_error("gg\t-\t1\n", 1, ERR_BAD_HEX);
        assert_parse_error("0\t-\t1\n", 1, ERR_BAD_HEX);
        assert_parse_error("00\tgg\t1\n", 1, ERR_BAD_HEX);
        assert_parse_error("00\t0\t1\n", 1, ERR_BAD_HEX);
        assert_second_line_error("aa\t-\t1\nzz\t-\t2\n", ERR_BAD_HEX);
    }

    #[test]
    fn parse_reports_invalid_ssid_lengths() {
        assert_parse_error("\t-\t1\n", 1, ERR_SSID_LENGTH);
        assert_parse_error(&format!("{}\t-\t1\n", "42".repeat(33)), 1, ERR_SSID_LENGTH);
    }

    #[test]
    fn parse_reports_invalid_passphrases() {
        assert_parse_error("00\t61616161616161\t1\n", 1, ERR_PASSPHRASE_LENGTH);
        assert_parse_error(
            &format!("00\t{}\t1\n", "61".repeat(64)),
            1,
            ERR_PASSPHRASE_LENGTH,
        );
        assert_parse_error("00\t616161616161617f\t1\n", 1, ERR_PASSPHRASE_PRINTABLE);
        assert_parse_error("00\t0101616161616161\t1\n", 1, ERR_PASSPHRASE_PRINTABLE);
    }

    #[test]
    fn parse_reports_bad_last_used_numbers() {
        assert_parse_error("00\t-\t\n", 1, ERR_LAST_USED);
        assert_parse_error("00\t-\t+1\n", 1, ERR_LAST_USED);
        assert_parse_error("00\t-\t-1\n", 1, ERR_LAST_USED);
        assert_parse_error("00\t-\t 1\n", 1, ERR_LAST_USED);
        assert_parse_error("00\t-\tone\n", 1, ERR_LAST_USED);
        assert_parse_error("00\t-\t18446744073709551616\n", 1, ERR_LAST_USED);
    }

    #[test]
    fn parse_skips_bad_lines_and_keeps_good_ones() {
        let text = "00\t-\t1\nbad\t-\tnope\nff\t6161616161616161\t3\n";
        let (book, errors) = Book::parse(text);
        assert_eq!(book.len(), 2);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 2);
        assert_eq!(errors[0].message, ERR_BAD_HEX);
    }

    #[test]
    fn parse_duplicate_ssid_keeps_later_entry() {
        let text = "6161\t-\t1\n6161\t6262626262626262\t2\n";
        let (book, errors) = Book::parse(text);
        assert!(errors.is_empty());
        assert_eq!(book.len(), 1);
        assert_eq!(book.entries()[0].last_used, 2);
        assert_eq!(
            book.entries()[0].passphrase.as_ref().map(Secret::expose),
            Some("bbbbbbbb")
        );
    }

    #[test]
    fn upsert_validates_and_replaces_same_ssid() {
        let mut book = Book::new();
        book.upsert(open_entry(b"net", 1)).unwrap();
        book.upsert(secured_entry(b"net", "eightchar", 2)).unwrap();
        assert_eq!(book.len(), 1);
        assert_eq!(book.entries()[0].last_used, 2);

        assert_eq!(book.upsert(open_entry(&[], 3)), Err(BookError::InvalidSsid));
        assert_eq!(
            book.upsert(open_entry(&[0_u8; 33], 3)),
            Err(BookError::InvalidSsid)
        );
    }

    #[test]
    fn passphrase_boundaries_and_printable_ascii() {
        assert!(Secret::new("1234567".to_owned()).is_err());
        assert!(Secret::new("12345678".to_owned()).is_ok());
        assert!(Secret::new("a".repeat(63)).is_ok());
        assert!(Secret::new("a".repeat(64)).is_err());
        assert!(Secret::new("        ".to_owned()).is_ok());
        assert!(Secret::new("1234567\x7f".to_owned()).is_err());
        assert!(Secret::new("1234567\n".to_owned()).is_err());
        assert!(Secret::new("hélloooo".to_owned()).is_err());
    }

    #[test]
    fn ssid_boundaries_are_enforced() {
        assert!(Book::new().upsert(open_entry(&[], 1)).is_err());
        assert!(Book::new().upsert(open_entry(b"a", 1)).is_ok());
        assert!(Book::new().upsert(open_entry(&[b'a'; 32], 1)).is_ok());
        assert!(Book::new().upsert(open_entry(&[b'a'; 33], 1)).is_err());
    }

    #[test]
    fn eviction_at_65_entries_removes_smallest_last_used() {
        let mut book = Book::new();
        for value in 1..=64_u8 {
            book.upsert(open_entry(&[value], u64::from(value))).unwrap();
        }

        book.upsert(open_entry(&[65], 65)).unwrap();

        assert_eq!(book.len(), 64);
        assert!(!book
            .entries()
            .iter()
            .any(|entry| entry.ssid.as_slice() == [1]));
        assert!(book
            .entries()
            .iter()
            .any(|entry| entry.ssid.as_slice() == [65]));
        assert!(!book.forget(&[1]));
        assert!(book.forget(&[2]));
    }

    #[test]
    fn best_match_prefers_strongest_signal() {
        let mut book = Book::new();
        book.upsert(one_byte_entry(b'a', Some("aaaaaaaa"), 1))
            .unwrap();
        book.upsert(one_byte_entry(b'b', Some("bbbbbbbb"), 100))
            .unwrap();

        let visible = vec![(vec![b'a'], -30), (vec![b'b'], -90)];
        let best = book.best_match(&visible).map(|entry| entry.ssid.as_slice());
        assert_eq!(best, Some(&b"a"[..]));
    }

    #[test]
    fn best_match_breaks_signal_ties_by_higher_last_used() {
        let mut book = Book::new();
        book.upsert(one_byte_entry(b'a', Some("aaaaaaaa"), 5))
            .unwrap();
        book.upsert(one_byte_entry(b'b', Some("bbbbbbbb"), 9))
            .unwrap();

        let visible = vec![(vec![b'a'], -50), (vec![b'b'], -50)];
        let best = book.best_match(&visible).map(|entry| entry.ssid.as_slice());
        assert_eq!(best, Some(&b"b"[..]));
    }

    #[test]
    fn best_match_ignores_unknown_networks_and_handles_empty() {
        let mut book = Book::new();
        book.upsert(one_byte_entry(b'k', None, 10)).unwrap();

        assert!(book.best_match(&[]).is_none());
        assert!(book.best_match(&[(vec![b'u'], -10)]).is_none());

        let visible = vec![(vec![b'u'], -10), (vec![b'k'], -80)];
        assert_eq!(
            book.best_match(&visible).map(|entry| entry.ssid.as_slice()),
            Some(&b"k"[..])
        );
    }

    #[test]
    fn debug_output_never_reveals_passphrase() {
        let passphrase = "correct horse battery staple";
        let entry = Entry {
            ssid: vec![b'a'],
            passphrase: Some(Secret::new(passphrase.to_owned()).unwrap()),
            last_used: 42,
        };

        let entry_debug = format!("{entry:?}");
        assert!(!entry_debug.contains(passphrase));
        assert!(entry_debug.contains("Secret(***)"));

        let mut book = Book::new();
        book.upsert(entry).unwrap();
        let book_debug = format!("{book:?}");
        assert!(!book_debug.contains(passphrase));

        let secret_debug = format!("{:?}", Secret::new(passphrase.to_owned()).unwrap());
        assert_eq!(secret_debug, "Secret(***)");
    }

    #[test]
    fn non_utf8_ssid_round_trips() {
        let ssid = vec![0xff, 0x00, 0x7f, 0x41];
        let mut book = Book::new();
        book.upsert(Entry {
            ssid: ssid.clone(),
            passphrase: Some(Secret::new("p@ss w0rd".to_owned()).unwrap()),
            last_used: 123,
        })
        .unwrap();

        let rendered = book.render();
        let (parsed, errors) = Book::parse(&rendered);

        assert!(errors.is_empty());
        assert_eq!(parsed.render(), rendered);
        let parsed_entry = parsed
            .entries()
            .iter()
            .find(|entry| entry.ssid == ssid)
            .unwrap();
        assert_eq!(
            parsed_entry.passphrase.as_ref().map(Secret::expose),
            Some("p@ss w0rd")
        );
        assert_eq!(parsed_entry.last_used, 123);
    }

    #[test]
    fn forget_returns_false_for_missing_ssid() {
        let mut book = Book::new();
        assert!(!book.forget(b"m"));
        book.upsert(open_entry(b"m", 1)).unwrap();
        assert!(book.forget(b"m"));
        assert!(book.is_empty());
    }
}
