//! What can be played, and how to tell safely.
//!
//! A [`Source`] is a YouTube video or Short (only its 11-character id is kept, and it is only
//! ever played by YouTube's own player), a local file, or a direct media URL. Parsing is strict:
//! the host of a YouTube address must be exactly one of the known YouTube hosts, with no user
//! name, port or look-alike domain, and a file or URL must not carry a scheme that the media
//! engine would treat as something else (such as `ytdl://`).

use std::fmt;

/// File extensions accepted for local files and direct URLs.
pub const MEDIA_EXTENSIONS: [&str; 16] = [
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "ts", "ogv", "mp3", "ogg", "opus", "flac", "wav",
    "m4a", "aac", "mpg",
];

/// Longest address or path accepted, in bytes.
pub const MAX_INPUT: usize = 2048;

const YOUTUBE_HOSTS: [&str; 5] = [
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "youtu.be",
    "www.youtube-nocookie.com",
];

/// Whether a YouTube item is an ordinary video or a Short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtKind {
    /// An ordinary video.
    Video,
    /// A YouTube Short.
    Short,
}

/// Something to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A YouTube video or Short, by id.
    YouTube {
        /// The 11-character video id.
        id: String,
        /// Video or Short.
        kind: YtKind,
    },
    /// A local file, by absolute path.
    File(String),
    /// A direct media address (https, or http to this computer).
    Url(String),
}

/// Why an input was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Nothing, or too long.
    BadLength,
    /// Control characters in the text.
    BadCharacters,
    /// A YouTube address that is not a valid video, Short or embed link.
    BadYouTube(String),
    /// A scheme or host that is not allowed.
    NotAllowed(String),
    /// Not a media file type we play.
    NotMedia,
    /// A file path that is not absolute.
    NotAbsolute,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::BadLength => write!(f, "empty or too long"),
            ParseError::BadCharacters => write!(f, "contains control characters"),
            ParseError::BadYouTube(m) => write!(f, "not a YouTube video address: {m}"),
            ParseError::NotAllowed(m) => write!(f, "not allowed: {m}"),
            ParseError::NotMedia => write!(f, "not a media file type"),
            ParseError::NotAbsolute => write!(f, "give the full path, starting with /"),
        }
    }
}

impl std::error::Error for ParseError {}

/// True for a valid YouTube video id: exactly 11 of letters, digits, `_` and `-`.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn has_media_extension(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// Splits `scheme://rest` and returns the scheme (lower case) and the rest.
fn split_scheme(s: &str) -> Option<(String, &str)> {
    let (scheme, rest) = s.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
    {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), rest))
}

/// The authority (host) and the path-and-query of the part after `://`.
fn split_authority(rest: &str) -> (&str, &str) {
    match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    }
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn youtube(host: &str, path_query: &str) -> Result<Source, ParseError> {
    let path_query = path_query.split('#').next().unwrap_or("");
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let bad = |m: &str| Err(ParseError::BadYouTube(m.to_string()));
    let (id, kind) = if host == "youtu.be" {
        match segments.as_slice() {
            [id] => (*id, YtKind::Video),
            _ => return bad("a youtu.be link has one id"),
        }
    } else {
        match segments.as_slice() {
            ["watch"] => match query_param(query, "v") {
                Some(v) => (v, YtKind::Video),
                None => return bad("a watch link needs v=ID"),
            },
            ["shorts", id] => (*id, YtKind::Short),
            ["embed", id] | ["live", id] | ["v", id] => (*id, YtKind::Video),
            _ => return bad("only watch, shorts and embed links can be played"),
        }
    };
    if is_video_id(id) {
        Ok(Source::YouTube {
            id: id.to_string(),
            kind,
        })
    } else {
        bad("the video id must be 11 letters, digits, _ or -")
    }
}

/// Reads what the person typed or pasted.
///
/// Accepted: `https://www.youtube.com/watch?v=ID`, `https://youtu.be/ID`,
/// `https://www.youtube.com/shorts/ID`, an embed link, `yt:ID` for a bare id, an absolute path to
/// a media file, or an `https` address of a media file (plain `http` only to this computer).
///
/// # Errors
/// [`ParseError`] saying why the input cannot be played.
pub fn parse(input: &str) -> Result<Source, ParseError> {
    let s = input.trim();
    if s.is_empty() || s.len() > MAX_INPUT {
        return Err(ParseError::BadLength);
    }
    if s.chars().any(char::is_control) {
        return Err(ParseError::BadCharacters);
    }
    if let Some(id) = s.strip_prefix("yt:") {
        return if is_video_id(id) {
            Ok(Source::YouTube {
                id: id.to_string(),
                kind: YtKind::Video,
            })
        } else {
            Err(ParseError::BadYouTube(
                "yt: needs an 11-character id".into(),
            ))
        };
    }
    if let Some((scheme, rest)) = split_scheme(s) {
        let (authority, path_query) = split_authority(rest);
        if authority.contains('@') {
            return Err(ParseError::NotAllowed("addresses with a user name".into()));
        }
        let host = authority.to_ascii_lowercase();
        if YOUTUBE_HOSTS.contains(&host.as_str()) {
            if scheme != "https" {
                return Err(ParseError::NotAllowed("YouTube links must be https".into()));
            }
            return youtube(&host, path_query);
        }
        if host.contains("youtube") || host.contains("youtu.be") {
            return Err(ParseError::BadYouTube(format!(
                "unknown YouTube host {host}"
            )));
        }
        let loopback = host == "127.0.0.1"
            || host == "localhost"
            || host.starts_with("127.0.0.1:")
            || host.starts_with("localhost:")
            || host == "[::1]";
        let ok_scheme = scheme == "https" || (scheme == "http" && loopback);
        if !ok_scheme {
            return Err(ParseError::NotAllowed(format!("the {scheme} scheme")));
        }
        let path = path_query.split(['?', '#']).next().unwrap_or("");
        if !has_media_extension(path) {
            return Err(ParseError::NotMedia);
        }
        return Ok(Source::Url(s.to_string()));
    }
    // A local path. The media engine would read "name://..." as a protocol, so refuse that.
    if s.contains("://") {
        return Err(ParseError::NotAllowed("a path containing ://".into()));
    }
    if !s.starts_with('/') {
        return Err(ParseError::NotAbsolute);
    }
    if !has_media_extension(s) {
        return Err(ParseError::NotMedia);
    }
    Ok(Source::File(s.to_string()))
}

impl Source {
    /// A short name for lists and the title bar.
    pub fn label(&self) -> String {
        match self {
            Source::YouTube { id, kind } => match kind {
                YtKind::Short => format!("YouTube Short {id}"),
                YtKind::Video => format!("YouTube {id}"),
            },
            Source::File(p) => p.rsplit('/').next().unwrap_or(p).to_string(),
            Source::Url(u) => u
                .rsplit('/')
                .next()
                .unwrap_or(u)
                .split('?')
                .next()
                .unwrap_or(u)
                .to_string(),
        }
    }

    /// True for a YouTube Short.
    pub fn is_short(&self) -> bool {
        matches!(
            self,
            Source::YouTube {
                kind: YtKind::Short,
                ..
            }
        )
    }

    /// True for anything played by YouTube's own player.
    pub fn is_youtube(&self) -> bool {
        matches!(self, Source::YouTube { .. })
    }

    /// The canonical address of a YouTube item, for showing to the person.
    pub fn watch_url(&self) -> Option<String> {
        match self {
            Source::YouTube {
                id,
                kind: YtKind::Short,
            } => Some(format!("https://www.youtube.com/shorts/{id}")),
            Source::YouTube {
                id,
                kind: YtKind::Video,
            } => Some(format!("https://www.youtube.com/watch?v={id}")),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(id: &str, kind: YtKind) -> Source {
        Source::YouTube {
            id: id.into(),
            kind,
        }
    }

    #[test]
    fn every_normal_youtube_address_gives_the_same_id() {
        let id = "dQw4w9WgXcQ";
        for u in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ&t=10s",
            "https://m.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "HTTPS://WWW.YOUTUBE.COM/watch?v=dQw4w9WgXcQ#frag",
            "  https://youtu.be/dQw4w9WgXcQ  ",
            "yt:dQw4w9WgXcQ",
        ] {
            assert_eq!(parse(u).unwrap(), yt(id, YtKind::Video), "{u}");
        }
    }

    #[test]
    fn shorts_are_recognised_and_kept_apart_from_videos() {
        let s = parse("https://www.youtube.com/shorts/abcDEF_-123").unwrap();
        assert_eq!(s, yt("abcDEF_-123", YtKind::Short));
        assert!(s.is_short() && s.is_youtube());
        assert_eq!(
            s.watch_url().unwrap(),
            "https://www.youtube.com/shorts/abcDEF_-123"
        );
        assert!(!parse("https://youtu.be/abcDEF_-123").unwrap().is_short());
    }

    #[test]
    fn look_alike_hosts_user_names_ports_and_plain_http_are_refused() {
        for u in [
            "https://youtube.com.evil.com/watch?v=dQw4w9WgXcQ",
            "https://evilyoutube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com@evil.com/watch?v=dQw4w9WgXcQ",
            "https://evil.com@www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com:8443/watch?v=dQw4w9WgXcQ",
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "ftp://www.youtube.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert!(parse(u).is_err(), "{u}");
        }
    }

    #[test]
    fn bad_ids_and_unsupported_youtube_pages_are_refused() {
        for u in [
            "https://www.youtube.com/watch",
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQxx",
            "https://www.youtube.com/watch?v=dQw4w9Wg;Q",
            "https://www.youtube.com/results?search_query=cats",
            "https://www.youtube.com/@channel",
            "https://www.youtube.com/playlist?list=PL123",
            "https://youtu.be/",
            "https://youtu.be/a/b",
            "yt:tooshort",
        ] {
            assert!(matches!(parse(u), Err(ParseError::BadYouTube(_))), "{u}");
        }
    }

    #[test]
    fn files_must_be_absolute_media_paths_without_a_protocol() {
        assert_eq!(
            parse("/home/me/clip.MP4").unwrap(),
            Source::File("/home/me/clip.MP4".into())
        );
        assert_eq!(
            parse("/mouli/data/music/song.flac").unwrap().label(),
            "song.flac"
        );
        assert_eq!(parse("clip.mp4"), Err(ParseError::NotAbsolute));
        assert_eq!(parse("../clip.mp4"), Err(ParseError::NotAbsolute));
        assert_eq!(parse("/home/me/notes.txt"), Err(ParseError::NotMedia));
        assert_eq!(
            parse("/home/me/.mp4"),
            Err(ParseError::NotMedia),
            "no name before the extension"
        );
        assert!(matches!(
            parse("/tmp/ytdl://x.mp4"),
            Err(ParseError::NotAllowed(_))
        ));
        assert!(matches!(
            parse("ytdl://dQw4w9WgXcQ"),
            Err(ParseError::NotAllowed(_))
        ));
        assert!(matches!(
            parse("edl://a.mp4"),
            Err(ParseError::NotAllowed(_))
        ));
        assert!(matches!(
            parse("file:///etc/passwd"),
            Err(ParseError::NotAllowed(_))
        ));
    }

    #[test]
    fn direct_urls_are_https_media_or_plain_http_to_this_computer_only() {
        assert_eq!(
            parse("https://cdn.example.org/a/b.webm?x=1").unwrap(),
            Source::Url("https://cdn.example.org/a/b.webm?x=1".into())
        );
        assert_eq!(
            parse("https://cdn.example.org/a/b.webm?x=1")
                .unwrap()
                .label(),
            "b.webm"
        );
        assert!(parse("http://127.0.0.1:8080/v.mp4").is_ok());
        assert!(parse("http://localhost/v.mp4").is_ok());
        assert!(matches!(
            parse("http://example.org/v.mp4"),
            Err(ParseError::NotAllowed(_))
        ));
        assert_eq!(
            parse("https://example.org/page.html"),
            Err(ParseError::NotMedia)
        );
        assert_eq!(parse("https://example.org/"), Err(ParseError::NotMedia));
    }

    #[test]
    fn empty_long_and_control_character_input_is_refused() {
        assert_eq!(parse(""), Err(ParseError::BadLength));
        assert_eq!(parse("   "), Err(ParseError::BadLength));
        assert_eq!(
            parse(&format!("/{}.mp4", "a".repeat(MAX_INPUT))),
            Err(ParseError::BadLength)
        );
        assert_eq!(parse("/a\n.mp4"), Err(ParseError::BadCharacters));
        assert_eq!(parse("/a\0.mp4"), Err(ParseError::BadCharacters));
    }

    #[test]
    fn video_ids_have_exactly_eleven_safe_characters() {
        assert!(is_video_id("dQw4w9WgXcQ") && is_video_id("-_-_-_-_-_-"));
        assert!(
            !is_video_id("")
                && !is_video_id("dQw4w9WgXc")
                && !is_video_id("dQw4w9WgXcQQ")
                && !is_video_id("dQw4w9Wg Qx")
                && !is_video_id("dQw4w9Wg\u{e9}Q")
        );
    }

    #[test]
    fn labels_and_watch_urls() {
        assert_eq!(
            yt("dQw4w9WgXcQ", YtKind::Video).label(),
            "YouTube dQw4w9WgXcQ"
        );
        assert_eq!(
            yt("dQw4w9WgXcQ", YtKind::Short).label(),
            "YouTube Short dQw4w9WgXcQ"
        );
        assert_eq!(
            yt("dQw4w9WgXcQ", YtKind::Video).watch_url().unwrap(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(Source::File("/a.mp4".into()).watch_url(), None);
    }

    #[test]
    fn nothing_panics_on_odd_input() {
        for s in [
            "://",
            "a://",
            "https://",
            "https:///watch?v=x",
            "yt:",
            "?",
            "#",
            "//",
            "\u{1F600}",
            "https://youtu.be/%00",
            "https://www.youtube.com/shorts//",
        ] {
            let _ = parse(s);
        }
    }
}
