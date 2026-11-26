//! Limits on one answer from the local model server: how many tokens at most, and the strings that
//! end it. Set on the kernel command line (`mouli.reply=300`, `mouli.stop=END,###`).
//!
//! What this does not do: it only reaches the local model server. A hosted model has its own fixed
//! limit. A stop string is cut from the answer by the server; a model that never writes one is
//! stopped by the token limit alone.

/// Most tokens in one answer when nothing is set.
pub const DEFAULT_REPLY_TOKENS: u32 = 512;
/// Fewest tokens an answer may be limited to.
pub const MIN_REPLY_TOKENS: u32 = 16;
/// Most stop strings kept (the server allows only a few).
pub const MAX_STOPS: usize = 4;
/// Longest stop string kept, in characters.
pub const MAX_STOP_CHARS: usize = 32;

fn value<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_whitespace()
        .rev()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// The token limit for one answer: `mouli.reply=N`, else [`DEFAULT_REPLY_TOKENS`]. Never below
/// [`MIN_REPLY_TOKENS`] and never above half of the context `ctx`, so the conversation keeps room.
#[must_use]
pub fn reply_tokens(cmdline: &str, ctx: u32) -> u32 {
    let wanted = value(cmdline, "mouli.reply")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_REPLY_TOKENS);
    let ceiling = (ctx / 2).max(MIN_REPLY_TOKENS);
    wanted.clamp(MIN_REPLY_TOKENS, ceiling)
}

/// The stop strings: `mouli.stop=a,b,c` (comma-separated; `\n` stands for a new line). Empty
/// strings, strings longer than [`MAX_STOP_CHARS`] and strings past [`MAX_STOPS`] are dropped.
#[must_use]
pub fn stops(cmdline: &str) -> Vec<String> {
    value(cmdline, "mouli.stop")
        .map(|v| {
            v.split(',')
                .map(|s| s.replace("\\n", "\n"))
                .filter(|s| !s.is_empty() && s.chars().count() <= MAX_STOP_CHARS)
                .take(MAX_STOPS)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_limit_has_a_default_and_is_kept_inside_sensible_bounds() {
        assert_eq!(reply_tokens("quiet mouli.text=1", 4096), 512);
        assert_eq!(reply_tokens("mouli.reply=300", 4096), 300);
        assert_eq!(
            reply_tokens("mouli.reply=5", 4096),
            16,
            "not below the minimum"
        );
        assert_eq!(
            reply_tokens("mouli.reply=99999", 4096),
            2048,
            "half of the context"
        );
        assert_eq!(reply_tokens("mouli.reply=99999", 512), 256);
        assert_eq!(
            reply_tokens("mouli.reply=abc", 4096),
            512,
            "unreadable falls back"
        );
        assert_eq!(reply_tokens("mouli.reply=", 4096), 512);
        assert_eq!(reply_tokens("mouli.reply=-3", 4096), 512);
        // The last value wins, like other kernel options; a tiny context still allows the minimum.
        assert_eq!(reply_tokens("mouli.reply=100 mouli.reply=200", 4096), 200);
        assert_eq!(reply_tokens("mouli.reply=100", 8), 16);
        // A similarly named option is not mistaken for it.
        assert_eq!(reply_tokens("xmouli.reply=100 mouli.replyx=100", 4096), 512);
    }

    #[test]
    fn stop_strings_are_split_cleaned_and_limited() {
        assert!(stops("quiet").is_empty());
        assert_eq!(stops("mouli.stop=END,###"), vec!["END", "###"]);
        assert_eq!(stops("mouli.stop=a\\nb"), vec!["a\nb"], "\\n is a new line");
        assert_eq!(stops("mouli.stop=,,x,"), vec!["x"], "empty ones dropped");
        let long = "y".repeat(33);
        assert_eq!(
            stops(&format!("mouli.stop={long},ok")),
            vec!["ok"],
            "too long dropped"
        );
        assert_eq!(stops("mouli.stop=1,2,3,4,5,6").len(), MAX_STOPS);
        assert!(stops("mouli.stop=").is_empty());
    }
}
