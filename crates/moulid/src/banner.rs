//! The logo shown first when MOULI OS starts.
//!
//! Plain ASCII only, so it looks the same on the serial console, the text console and the
//! framebuffer console, and at most 78 columns wide so it fits an 80-column screen.

/// The product name in large letters.
const LOGO: [&str; 6] = [
    "#     #   #####   #     #  #        #######     #####    ######",
    "##   ##  #     #  #     #  #           #       #     #  #",
    "# # # #  #     #  #     #  #           #       #     #   #####",
    "#  #  #  #     #  #     #  #           #       #     #        #",
    "#     #  #     #  #     #  #           #       #     #        #",
    "#     #   #####    #####   #######  #######     #####   ######",
];

/// The author's name.
pub const AUTHOR: &str = "Chandra Mouli Krishna Moorthy";
/// The author's email address.
pub const EMAIL: &str = "cmklive@gmail.com";

const RULE: &str = "-------------------------------------------------------------------";

/// The whole banner for `version`: a blank line, the logo, and the author's details.
pub fn banner(version: &str) -> String {
    let mut s = String::from("\n");
    for l in LOGO {
        s.push_str("  ");
        s.push_str(l);
        s.push('\n');
    }
    s.push('\n');
    for l in [
        RULE.to_string(),
        format!("  MOULI OS {version}  -  AI-native operating system"),
        format!("  Author : {AUTHOR}"),
        format!("  Email  : {EMAIL}"),
        RULE.to_string(),
    ] {
        s.push_str("  ");
        s.push_str(&l);
        s.push('\n');
    }
    s
}

// ------------------------------------------------------------------ colour banner

/// The console's 16 colours, set to Material Design colours (slot, `rrggbb`). Slot 0 is the
/// background (Material dark surface) and slot 7 the default text. The banner uses only slots 0 to
/// 7: after a bright colour (slots 8 to 15) the Linux console keeps its "bright" state for the
/// colours that follow, which washes them out. Slots 8 to 15 are lighter tones for other programs.
const PALETTE: [&str; 16] = [
    "121212", // 0  surface (background)
    "ef5350", // 1  red 400
    "66bb6a", // 2  green 400
    "ffca28", // 3  amber 400
    "42a5f5", // 4  blue 400
    "90caf9", // 5  blue 200 (the desktop's primary colour)
    "757575", // 6  grey 600 (shadow and labels)
    "e0e0e0", // 7  on-surface (default text)
    "424242", // 8  grey 800
    "ff8a80", // 9  red A100
    "b9f6ca", // 10 green A100
    "ffe57f", // 11 amber A100
    "82b1ff", // 12 blue A100
    "e1bee7", // 13 purple 100
    "84ffff", // 14 cyan A100
    "ffffff", // 15 white
];

/// The escape codes that load [`PALETTE`] into the Linux console (`ESC ] P n rrggbb`; the slot
/// number is the same as the colour number in `ESC [ 3n m`).
pub fn palette_sequence() -> String {
    let mut s = String::new();
    for (slot, rgb) in PALETTE.iter().enumerate() {
        s.push_str(&format!("\x1b]P{slot:X}{rgb}"));
    }
    // Repaint the screen with the new background.
    s.push_str("\x1b[0m\x1b[H\x1b[2J");
    s
}

/// Whether the colour banner suits this console: the screen is the main console (not a serial
/// line), and the person has not asked for plain text with `mouli.plain=1`.
pub fn use_color(cmdline: &str) -> bool {
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    words
        .iter()
        .any(|w| *w == "console=tty0" || *w == "console=tty1")
        && !words.iter().any(|w| w.starts_with("console=ttyS"))
        && !words.contains(&"mouli.plain=1")
}

/// The letters of the logo in block characters with a shadow (6 rows each).
const BIG: [(&str, [&str; 6], u8); 5] = [
    (
        "M",
        [
            "\u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2588}\u{2557}",
            "\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551} \u{255a}\u{2550}\u{255d} \u{2588}\u{2588}\u{2551}",
            "\u{255a}\u{2550}\u{255d}     \u{255a}\u{2550}\u{255d}",
        ],
        4,
    ),
    (
        "O",
        [
            " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} ",
            "\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}",
            "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "\u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}",
            " \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d} ",
        ],
        1,
    ),
    (
        "U",
        [
            "\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2557}",
            "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}",
            "\u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}",
            " \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d} ",
        ],
        3,
    ),
    (
        "L",
        [
            "\u{2588}\u{2588}\u{2557}     ",
            "\u{2588}\u{2588}\u{2551}     ",
            "\u{2588}\u{2588}\u{2551}     ",
            "\u{2588}\u{2588}\u{2551}     ",
            "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}",
            "\u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}",
        ],
        2,
    ),
    (
        "I",
        [
            "\u{2588}\u{2588}\u{2557}",
            "\u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}",
            "\u{2588}\u{2588}\u{2551}",
            "\u{255a}\u{2550}\u{255d}",
        ],
        4,
    ),
];

/// The SGR code that selects palette slot `n` (0 to 7) as the text colour. Slots 8 to 15 are not
/// used here (see [`PALETTE`]).
fn fg(n: u8) -> String {
    debug_assert!(n < 8);
    format!("\x1b[{}m", 30 + (n & 7))
}

const RESET: &str = "\x1b[0m";

/// The colour banner for `version`: a block-letter logo (each letter its own Material colour, with
/// a grey shadow), then the product line and the author's details. Use it only when
/// [`use_color`] is true, after printing [`palette_sequence`].
pub fn banner_color(version: &str) -> String {
    let mut s = String::from("\n");
    for row in 0..6 {
        s.push_str("  ");
        for (_, glyph, colour) in &BIG {
            let mut current: Option<u8> = None;
            for ch in glyph[row].chars() {
                let want = match ch {
                    '\u{2588}' => Some(*colour),
                    ' ' => current,
                    _ => Some(6),
                };
                if want != current {
                    if let Some(c) = want {
                        s.push_str(&fg(c));
                    }
                    current = want;
                }
                s.push(ch);
            }
        }
        s.push_str(RESET);
        s.push('\n');
    }
    s.push('\n');
    let rule = format!("  {}{RULE}{RESET}\n", fg(6));
    s.push_str(&rule);
    s.push_str(&format!(
        "  {}MOULI OS {version}{RESET}  -  AI-native operating system\n",
        fg(5)
    ));
    s.push_str(&format!("  {}Author :{RESET} {AUTHOR}\n", fg(6)));
    s.push_str(&format!("  {}Email  :{RESET} {EMAIL}\n", fg(6)));
    s.push_str(&rule);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_the_product_the_author_and_the_email() {
        let b = banner("0.1.0");
        assert!(b.contains("MOULI OS 0.1.0"), "{b}");
        assert!(b.contains("Chandra Mouli Krishna Moorthy"), "{b}");
        assert!(b.contains("cmklive@gmail.com"), "{b}");
    }

    #[test]
    fn it_is_plain_ascii_and_fits_an_eighty_column_screen() {
        let b = banner("0.1.0");
        assert!(b.is_ascii(), "{b}");
        for l in b.lines() {
            assert!(l.len() <= 78, "{} columns: {l}", l.len());
        }
        assert!(b.lines().count() >= 12, "it is a big logo");
    }

    #[test]
    fn it_never_prints_a_phrase_the_boot_tests_wait_for() {
        // The CI boot tests look for lines such as "MOULI ready" or "MOULI hardware:". The
        // banner must not be able to satisfy or confuse them.
        let b = banner("0.1.0");
        for phrase in [
            "MOULI ready",
            "MOULI hardware:",
            "MOULI selftest:",
            "MOULI chat reply:",
            "MOULI network:",
            "MOULI data:",
            "MOULI wifi",
            "MOULI clock:",
        ] {
            assert!(!b.contains(phrase), "{phrase}");
        }
    }

    #[test]
    fn every_logo_row_has_the_same_left_edge_and_the_letters_are_drawn() {
        for l in LOGO {
            assert!(l.starts_with('#'), "{l}");
            assert!(l.chars().all(|c| c == '#' || c == ' '));
        }
        // The first row of "MOULI OS" has ink in all eight letters' columns.
        assert!(LOGO[0].matches('#').count() >= 20);
    }

    fn visible(s: &str) -> String {
        // Remove escape sequences: CSI (ESC [ ... letter) and the console's palette codes.
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                match it.peek() {
                    Some('[') => {
                        it.next();
                        for n in it.by_ref() {
                            if n.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        for _ in 0..9 {
                            it.next();
                        }
                    }
                    _ => {}
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn the_colour_banner_keeps_the_name_the_author_and_the_email_in_one_piece() {
        let b = banner_color("0.1.0");
        assert!(b.contains("MOULI OS 0.1.0"), "{b}");
        assert!(b.contains("Chandra Mouli Krishna Moorthy"), "{b}");
        assert!(b.contains("cmklive@gmail.com"), "{b}");
    }

    #[test]
    fn the_colour_banner_fits_eighty_columns_and_draws_five_big_letters() {
        let v = visible(&banner_color("0.1.0"));
        for l in v.lines() {
            assert!(
                l.chars().count() <= 78,
                "{} columns: {l}",
                l.chars().count()
            );
        }
        let rows: Vec<&str> = v.lines().filter(|l| l.contains('\u{2588}')).collect();
        assert_eq!(
            rows.len(),
            5,
            "rows with filled blocks (the sixth is all shadow)"
        );
        assert!(v.lines().count() >= 12);
    }

    #[test]
    fn the_colour_banner_never_prints_a_phrase_the_boot_tests_wait_for() {
        let b = visible(&banner_color("0.1.0"));
        for phrase in [
            "MOULI ready",
            "MOULI hardware:",
            "MOULI selftest:",
            "MOULI chat reply:",
            "MOULI network:",
            "MOULI data:",
            "MOULI wifi",
            "MOULI bluetooth",
        ] {
            assert!(!b.contains(phrase), "{phrase}");
        }
    }

    #[test]
    fn no_colour_leaks_past_the_end_of_a_line() {
        // The last escape code on every line that has one must be a reset.
        for l in banner_color("0.1.0").lines().filter(|l| l.contains('\x1b')) {
            let last = &l[l.rfind('\x1b').unwrap()..];
            assert!(last.starts_with(RESET), "{l:?}");
        }
    }

    #[test]
    fn the_palette_has_sixteen_valid_colours() {
        let p = palette_sequence();
        assert_eq!(p.matches("\x1b]P").count(), 16);
        for rgb in PALETTE {
            assert_eq!(rgb.len(), 6);
            assert!(
                rgb.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{rgb}"
            );
        }
        assert!(p.contains("\x1b]P0121212"), "background is Material dark");
        assert!(p.contains("\x1b]P1ef5350"), "slot 1 is red 400");
        assert!(p.contains("\x1b]P442a5f5"), "slot 4 is blue 400");
    }

    #[test]
    fn the_banner_uses_only_the_normal_colour_slots() {
        // SGR 90 to 97 would switch on the console's bright state and wash out what follows.
        let b = banner_color("0.1.0");
        assert!(!b.contains("\x1b[9"), "{b:?}");
        assert!(!b.contains("\x1b[1m"), "no bold");
        for code in 30..=37 {
            let _ = code; // every colour code used is 30..=37 by construction of fg()
        }
        assert_eq!(fg(4), "\x1b[34m");
        assert_eq!(fg(1), "\x1b[31m");
    }

    #[test]
    fn colour_is_used_only_on_the_screen_console() {
        assert!(use_color("quiet loglevel=3 console=tty0"));
        assert!(use_color("console=tty1"));
        assert!(!use_color(
            "loglevel=4 console=ttyS0,115200 mouli.selftest=1"
        ));
        assert!(
            !use_color("console=ttyS0 console=tty0"),
            "a serial line is attached too"
        );
        assert!(!use_color("console=tty0 mouli.plain=1"));
        assert!(!use_color(""));
    }
}
