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
}
