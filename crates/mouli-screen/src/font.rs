//! Looking up glyphs.

use crate::font_data::{BASIC, BLOCK, BOX};

/// Glyph width and height in pixels.
pub const GLYPH: usize = 8;

/// The 8x8 bitmap for `ch` (8 rows; least significant bit is the leftmost pixel). Characters the
/// font does not have are shown as `?`.
pub fn glyph(ch: char) -> &'static [u8; 8] {
    let c = ch as u32;
    let found = match c {
        0x00..=0x7F => BASIC.get(c as usize),
        0x2500..=0x257F => BOX.get((c - 0x2500) as usize),
        0x2580..=0x259F => BLOCK.get((c - 0x2580) as usize),
        _ => None,
    };
    found.unwrap_or(&BASIC[b'?' as usize])
}

/// True when the font has a real glyph for `ch`.
pub fn has_glyph(ch: char) -> bool {
    matches!(ch as u32, 0x00..=0x7F | 0x2500..=0x259F)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(ch: char) -> Vec<String> {
        glyph(ch)
            .iter()
            .map(|b| {
                (0..8)
                    .map(|x| if b >> x & 1 == 1 { 'X' } else { '.' })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn capital_a_has_the_expected_shape() {
        // From the original header: 0x0C 0x1E 0x33 0x33 0x3F 0x33 0x33 0x00, least significant bit
        // leftmost (the reference renderer prints bit y of row x at column y).
        assert_eq!(
            rows('A'),
            vec![
                "..XX....", ".XXXX...", "XX..XX..", "XX..XX..", "XXXXXX..", "XX..XX..", "XX..XX..",
                "........"
            ]
        );
    }

    #[test]
    fn space_is_blank_and_full_block_is_solid() {
        assert!(glyph(' ').iter().all(|b| *b == 0));
        assert!(glyph('\u{2588}').iter().all(|b| *b == 0xFF));
        assert_eq!(
            glyph('\u{2500}')[4],
            0xFF,
            "thin horizontal line sits in row 4"
        );
    }

    #[test]
    fn unknown_characters_fall_back_to_a_question_mark() {
        assert_eq!(glyph('\u{4e2d}'), glyph('?'));
        assert!(!has_glyph('\u{4e2d}') && has_glyph('x') && has_glyph('\u{2502}'));
    }

    #[test]
    fn every_printable_ascii_character_is_distinct_from_blank() {
        for c in '!'..='~' {
            assert!(glyph(c).iter().any(|b| *b != 0), "{c:?} is blank");
        }
    }
}
