# mouli-player

The core of MOULI's video and YouTube player, in plain Rust with no dependencies. It decides what to
play and when; the programs that actually decode video and draw it are added by the OS image.

| Module | What it does |
|---|---|
| `source` | Reads what a person types or pastes: YouTube video, Short or embed links, `yt:ID`, an absolute media path, or an `https` media address. Strict: exact YouTube hosts only (no look-alike domains, user names or ports), 11-character ids, and no `ytdl://`-style schemes. |
| `queue` | The play queue (up to 1000 items) with shuffle, repeat off/one/all and previous. A shuffled repeat-all round never starts with the item just played. |
| `engine` | The player state machine. When an item ends the next one starts by itself; failed items are skipped but after three in a row it stops instead of looping; per-item time limits for previews; volume, mute, speed, seek; thumbs up, down and report. |
| `bridge` | A small local server (`127.0.0.1`, std only) that serves YouTube's official player page and passes commands out and events (ended, error, position) back, so the engine can play the next Short by itself. Locked down: secret per-run token in every path, `Host` and `Origin` checks, tiny bodies, strict content policy (scripts only from itself and YouTube), at most 8 connections, slow clients dropped. Also `Router`, which sends YouTube items to the page and everything else to mpv. |
| `mpv` | The `mpv` control protocol (one JSON message per line): commands out, events in, with a small bounded JSON reader. `mpv_args` starts mpv with `--ytdl=no`, no scripts and no config. |

## Shorts without scrolling

`Command::ShortsMode(true)` turns on autoplay and passes over ordinary YouTube videos, so a queue of
Shorts plays one after another with no input; with repeat-all it is an endless feed. Down and report
skip the item and are remembered for curation.

## YouTube: what is and is not done

YouTube items are only ever played by YouTube's own web player, with its ads and its rules. This
crate never fetches, extracts or decodes YouTube video, and mpv refuses to (it is started with its
YouTube hook off). The bridge serves the page that runs that player and hands the engine its events.

**Verified:** the server's rules and the whole command and event flow over real local sockets,
including a four-Short feed that plays hands-free and skips a Short YouTube refuses; and the page
script, run in Node against a fake YouTube player and fake network.

**Not verified:** the page in a real browser against real YouTube (nothing here has run a browser),
including whether YouTube accepts a player embedded on `http://127.0.0.1` (it may answer error 153
or 150/101 for some videos). **Not built yet:** the browser engine in the boot image (for example
WPE WebKit with `cog`) and the mpv and sound packages, the video and sound output, and the player
screen. Until then nothing can be shown or heard.

## Tests

`cargo test -p mouli-player` (72 tests plus a Node check of the page script: address parsing against look-alike and hostile input, queue and
shuffle properties, the state machine including endless-failure protection, the mpv message reader
against malformed and over-deep input, and a player driven end to end by mpv-style messages, and the bridge: token, host, origin, size and connection limits, slow clients, command order and long polling, port release, and hands-free Shorts through real sockets).
